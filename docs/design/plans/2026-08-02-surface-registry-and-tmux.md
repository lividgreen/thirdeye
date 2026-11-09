# Surface Registry and tmux Surface — Implementation Plan

**Goal:** Make the Surface seam actually pluggable — adding a surface becomes
adding a file and one registry line — and prove it with a tmux surface, which
works in every terminal emulator and over SSH.

**Architecture:** Three hardcodings are removed in order. The hook stops
capturing one named environment variable and starts capturing a declared
allowlist into `SessionRef.env`; the process-spawning `Runner` moves out of
`kitty.rs` and returns captured stdout, because tmux's whole addressing scheme
depends on reading the pane id it prints; and `daemon::run`'s `kitty_surface()`
becomes `surface::resolve()` over a preference-ordered registry. The ordering
rule is **most-nested wins**: an agent inside tmux inside kitty gets a tmux
pane, because the head must appear in the container the human is looking at,
and because over SSH kitty's control socket is unreachable from the remote host
while tmux's is not.

**Tech Stack:** Rust 2024, `anyhow`, `serde`/`serde_json`, `std::process`. No
new dependencies.

## Global Constraints

- Rust 2024 edition; new crates need `[lints] workspace = true`. No new crates
  are created by this plan.
- `unsafe_code` is `forbid` and `clippy::all` is `deny` at the workspace level.
  Lint failures are build failures.
- **`std::env::set_var` is `unsafe` in Rust 2024 and therefore unavailable.**
  Any function whose behaviour depends on the process environment must take a
  lookup closure so tests can drive it without touching the real environment.
  This is not a style preference; the alternative does not compile.
- Tests are colocated in `#[cfg(test)] mod tests` at the bottom of each module.
- Test names state the property being held, not the function being called.
- Comments explain *why* a choice was made over the obvious alternative; they
  do not restate the code.
- Every task ends green on all four gates: `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`,
  and `cargo build --workspace`.
- Session ids and workspace names are **untrusted**. tmux runs its
  `shell-command` argument through `sh`, so anything interpolated into it must
  be shell-quoted.
- Never rebuild only the binary. After Task 6, verification uses
  `cargo xtask install`.
- `docs/design/specs/` is not edited by plans; Task 6
  records what the spec needs to fold back in.

---

## File Structure

**Created:**

- `crates/thirdeye/src/daemon/surface/runner.rs` — the process-spawning seam,
  shared by every surface that drives an external program. Lives in its own
  file because `mod.rs` holds the seam and nothing else, and a `Runner` is not
  itself a `Surface`.
- `crates/thirdeye/src/daemon/surface/tmux.rs` — the tmux surface.
- `docs/tmux.md` — the manual verification procedure, mirroring `docs/kitty.md`.

**Modified:**

- `crates/thirdeye/src/daemon/surface/mod.rs` — gains `SURFACE_ENV`, `capture`,
  `ORDER`, `resolve`, `Surface::id`, and `impl Surface for Box<dyn Surface>`.
- `crates/thirdeye/src/daemon/surface/kitty.rs` — `Runner` moves out; gains
  `detects` and `from_env`.
- `crates/thirdeye/src/daemon/surface/null.rs` — gains `id`.
- `crates/thirdeye/src/session_ref.rs` — `kitty: Option<String>` becomes
  `env: BTreeMap<String, String>`.
- `crates/thirdeye/src/cmd_hook.rs:95-98` — captures the allowlist instead of
  one variable.
- `crates/thirdeye/src/daemon/mod.rs:66-72, 94-100, 384-393` — one
  `session_ref::load`, then `surface::resolve`.
- `crates/thirdeye/src/cmd_doctor.rs` — reports the surface this environment
  would get.
- `CLAUDE.md` — seam table, resolution rule, new gotcha.

---

## Task 1: Runner moves out and returns captured stdout

**Files:**
- Create: `crates/thirdeye/src/daemon/surface/runner.rs`
- Modify: `crates/thirdeye/src/daemon/surface/mod.rs` (add `pub mod runner;`)
- Modify: `crates/thirdeye/src/daemon/surface/kitty.rs` (delete the local
  `Runner`/`ProcessRunner`/`is_executable`, import from `runner`)
- Modify: `crates/thirdeye/src/daemon/mod.rs:40` (import path)

**Interfaces:**
- Produces: `daemon::surface::runner::Runner` with
  `fn run(&self, program: &str, args: &[String]) -> Result<String>` returning
  captured stdout trimmed, and `fn available(&self, program: &str) -> bool`.
  `daemon::surface::runner::ProcessRunner` implements it.
- Consumes: nothing.

- [ ] **Step 1: Write the failing test**

Create `crates/thirdeye/src/daemon/surface/runner.rs` containing only the test
module for now, so the test names the behaviour before the type exists:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdout_comes_back_because_a_pane_id_is_only_ever_printed() {
        // tmux's `split-window -P` prints the id of the pane it just made,
        // and that id is the only precise handle for closing it later. A
        // runner that discarded stdout would make the tmux surface impossible
        // to write, which is why this is the seam's contract and not an extra.
        let out = ProcessRunner
            .run("sh", &["-c".into(), "printf %s %3".into()])
            .unwrap();
        assert_eq!(out, "%3");
    }

    #[test]
    fn a_nonzero_exit_is_an_error_not_an_empty_string() {
        // A surface that read a failed launch as "it printed nothing" would
        // believe a window exists and never retry.
        assert!(
            ProcessRunner
                .run("sh", &["-c".into(), "exit 3".into()])
                .is_err()
        );
    }

    #[test]
    fn a_program_that_is_not_installed_is_not_available() {
        assert!(!ProcessRunner.available("thirdeye-no-such-program"));
        assert!(ProcessRunner.available("sh"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p thirdeye runner:: 2>&1 | tail -20`
Expected: compile error — `ProcessRunner` not found, and `surface::runner` is
not a module yet.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/thirdeye/src/daemon/surface/runner.rs`, above the test
module:

```rust
//! Running an external program, behind a seam.
//!
//! Shared by every surface that drives one — `kitten @` for kitty, `tmux` for
//! a pane. It sits in its own file rather than in `mod.rs` because `mod.rs`
//! holds the seam and nothing else, and a `Runner` is not a `Surface`.
//!
//! Not a seam for mocking's sake: neither `kitten @` nor `tmux` can run in
//! CI, and a surface whose only test was "we called something" would be
//! worthless. Asserting the exact argv is the part that *can* be checked, and
//! it is the part that is easy to get subtly wrong.

use std::fmt::Debug;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub trait Runner: Debug + Send {
    /// Run `program`, returning its captured stdout, trimmed.
    ///
    /// Returning stdout rather than discarding it is what makes the tmux
    /// surface possible: `split-window -P` prints the new pane's id, and
    /// nothing else ever tells us what it is. kitty ignores the value.
    fn run(&self, program: &str, args: &[String]) -> Result<String>;

    /// Whether `program` can be executed at all.
    fn available(&self, program: &str) -> bool;
}

#[derive(Debug, Default)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String> {
        let out = Command::new(program)
            .args(args)
            // stdout is captured rather than inherited, and stderr dropped: a
            // surface's chatter belongs in neither the journal nor the agent's
            // terminal. Failures are reported through the exit status.
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("running {program}"))?;
        if !out.status.success() {
            bail!("{program} exited with {}", out.status);
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn available(&self, program: &str) -> bool {
        // A PATH walk rather than spawning `program --version`: `available` is
        // asked on every refresh, and a process spawn per event is a cost the
        // hot path should not carry.
        std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths).any(|dir| {
                    let candidate = dir.join(program);
                    candidate.is_file() && is_executable(&candidate)
                })
            })
            .unwrap_or(false)
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}
```

- [ ] **Step 4: Register the module**

In `crates/thirdeye/src/daemon/surface/mod.rs`, change the module list to:

```rust
pub mod kitty;
pub mod null;
pub mod runner;
```

- [ ] **Step 5: Delete the duplicate from kitty.rs**

In `crates/thirdeye/src/daemon/surface/kitty.rs`, delete the `Runner` trait,
the `ProcessRunner` struct with its `impl`, and both `is_executable` functions
(lines 26–81 of the current file). Replace the import block at the top with:

```rust
use std::fmt::Debug;

use anyhow::{Context, Result, bail};

use super::runner::Runner;
use super::{Surface, ViewRequest};
```

`Context` is retained because `open`'s error path still uses it below; if
`cargo clippy` reports it unused, delete it.

- [ ] **Step 6: Adapt kitty's call sites and its test double**

In `KittySurface::open`, discard the returned stdout:

```rust
    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        if self.open {
            return Ok(());
        }
        self.runner.run("kitten", &self.launch_args(request))?;
        self.open = true;
        Ok(())
    }
```

In `KittySurface::close`, the final line must drop the `String`:

```rust
        self.runner.run("kitten", &self.close_args()).map(|_| ())
```

In kitty's `#[cfg(test)] mod tests`, change `Recorder`'s impl to the new
signature:

```rust
    impl Runner for Recorder {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().cloned());
            self.calls.lock().unwrap().push(call);
            if self.fail {
                bail!("planted failure");
            }
            Ok(String::new())
        }
        fn available(&self, _program: &str) -> bool {
            self.available
        }
    }
```

- [ ] **Step 7: Fix the daemon's import**

In `crates/thirdeye/src/daemon/mod.rs:40`, replace:

```rust
use surface::kitty::{KittySurface, ProcessRunner};
```

with:

```rust
use surface::kitty::KittySurface;
use surface::runner::ProcessRunner;
```

- [ ] **Step 8: Run the gates**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: PASS, including all pre-existing kitty tests unchanged.

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/thirdeye/src/daemon/surface/runner.rs \
        crates/thirdeye/src/daemon/surface/mod.rs \
        crates/thirdeye/src/daemon/surface/kitty.rs \
        crates/thirdeye/src/daemon/mod.rs
git commit -m "refactor(surface): share the runner, and let it return what it printed"
```

---

## Task 2: The hook records a declared environment allowlist

**Files:**
- Modify: `crates/thirdeye/src/daemon/surface/mod.rs` (add `SURFACE_ENV`, `capture`)
- Modify: `crates/thirdeye/src/session_ref.rs` (`kitty` field → `env` map)
- Modify: `crates/thirdeye/src/cmd_hook.rs:95-98`
- Modify: `crates/thirdeye/src/daemon/mod.rs:389-393` (`kitty_surface` reads the map)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `daemon::surface::SURFACE_ENV: &[&str]`
  - `daemon::surface::capture(lookup: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String>`
  - `session_ref::SessionRef { session: String, workspace: PathBuf, env: BTreeMap<String, String> }`
  - `session_ref::record(socket: &Path, session: &str, workspace: &Path, env: BTreeMap<String, String>) -> std::io::Result<()>`

- [ ] **Step 1: Write the failing tests for the allowlist**

Append to the `#[cfg(test)] mod tests` in
`crates/thirdeye/src/daemon/surface/mod.rs` (create the module if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capturing_takes_only_what_a_surface_declared() {
        // The log already holds prompt text and every path the agent touched.
        // Copying the agent's whole environment beside it would add API keys
        // and tokens to that pile for no reader's benefit.
        let env = capture(|key| match key {
            "TMUX" => Some("/tmp/tmux-1000/default,7,0".into()),
            "AWS_SECRET_ACCESS_KEY" => Some("hunter2".into()),
            _ => None,
        });
        assert_eq!(env.get("TMUX").map(String::as_str), Some("/tmp/tmux-1000/default,7,0"));
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn an_empty_variable_is_absent_rather_than_present_and_blank() {
        // Every surface treats "not set" as its own no-surface signal (§12).
        // An empty string that survived to the map would read as "set", and
        // the daemon would try to drive a terminal that is not there.
        let env = capture(|key| match key {
            "KITTY_LISTEN_ON" => Some(String::new()),
            _ => None,
        });
        assert!(env.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p thirdeye surface::tests 2>&1 | tail -20`
Expected: compile error — `capture` not found.

- [ ] **Step 3: Implement the allowlist**

Add to `crates/thirdeye/src/daemon/surface/mod.rs`, above the `Surface` trait:

```rust
use std::collections::BTreeMap;

/// The environment a surface may need, declared here and copied by the hook.
///
/// It lives beside the surfaces rather than in the hook because it is the
/// seam's contract with the hook: adding a surface must be adding a file and
/// a line here, never editing the hook. The hook merely copies what is listed.
///
/// An allowlist rather than the whole environment, because the recorded file
/// sits beside a log that already holds prompt text and every path the agent
/// touched. Copying `AWS_SECRET_ACCESS_KEY` in there would be a new class of
/// exposure for no reader's benefit.
pub const SURFACE_ENV: &[&str] = &[
    // A human naming a surface outranks detection. First, so it is visible
    // that the rest of this list is only consulted when nobody chose.
    "THIRDEYE_SURFACE",
    // tmux: `<socket-path>,<server-pid>,<session-index>`.
    "TMUX",
    // tmux: the agent's own pane, and therefore the pane we split.
    "TMUX_PANE",
    // kitty: which instance to drive.
    "KITTY_LISTEN_ON",
];

/// Copy the declared variables out of an environment.
///
/// Takes a lookup rather than reading `std::env` directly for a reason that
/// is not stylistic: `std::env::set_var` is `unsafe` in Rust 2024 and the
/// workspace forbids `unsafe`, so a test could not otherwise set up an
/// environment to capture from.
pub fn capture(lookup: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    SURFACE_ENV
        .iter()
        .filter_map(|key| {
            let value = lookup(key)?;
            // Empty is absent: every surface treats "not set" as its own
            // no-surface signal, and a blank string would read as set.
            (!value.is_empty()).then(|| ((*key).to_string(), value))
        })
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p thirdeye surface::tests 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Write the failing tests for the widened SessionRef**

In `crates/thirdeye/src/session_ref.rs`, **replace** the existing test
`the_kitty_address_travels_with_the_session` and the existing test
`a_pointer_written_before_kitty_was_recorded_still_loads` with:

```rust
    #[test]
    fn the_surface_environment_travels_with_the_session() {
        // Only the hook's environment has any of this, and the daemon
        // inherits systemd's. Both the present and the absent state must
        // round-trip, because absence is what §12 makes the "no surface"
        // signal.
        let socket = tmp("env").join("s.sock");
        let mut env = BTreeMap::new();
        env.insert("TMUX".to_string(), "/tmp/tmux-1000/default,7,0".to_string());
        env.insert("TMUX_PANE".to_string(), "%3".to_string());
        record(&socket, "s1", Path::new("/w"), env).unwrap();

        let found = load(&socket).unwrap();
        assert_eq!(found.env.get("TMUX_PANE").map(String::as_str), Some("%3"));

        record(&socket, "s1", Path::new("/w"), BTreeMap::new()).unwrap();
        assert!(load(&socket).unwrap().env.is_empty());
    }

    #[test]
    fn a_pointer_written_before_the_env_map_still_loads() {
        // Old files predate the field. A missing key must not read as corrupt,
        // which would make the daemon resolve the wrong workspace and report
        // nothing while everything else worked.
        let socket = tmp("older").join("s.sock");
        std::fs::write(path_for(&socket), br#"{"session":"s1","workspace":"/w"}"#).unwrap();
        let found = load(&socket).unwrap();
        assert_eq!(found.session, "s1");
        assert!(found.env.is_empty());
    }
```

Every other call to `record(...)` in that test module passes `None` as its
fourth argument; change each to `BTreeMap::new()`. There are four:
`a_hashed_socket_stem_still_recovers_the_real_session_id`,
`re_recording_the_same_pair_does_not_rewrite_the_file` (two calls), and
`a_changed_workspace_overwrites_the_stale_pair` (two calls).

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test -p thirdeye session_ref 2>&1 | tail -20`
Expected: compile error — `SessionRef` has no field `env`, and `record` takes
`Option<String>`.

- [ ] **Step 7: Widen SessionRef**

In `crates/thirdeye/src/session_ref.rs`, add `use std::collections::BTreeMap;`
to the imports, then replace the `kitty` field with:

```rust
    /// The parts of the agent's environment a surface needs.
    ///
    /// A map rather than a field per surface, because §5 says adding a surface
    /// is adding a file — not editing a struct that every other surface
    /// already depends on. The keys are exactly `surface::SURFACE_ENV`.
    ///
    /// Recorded for the same reason as the rest of this file: it exists only
    /// in the hook's environment, and the daemon has none of it. An empty map
    /// is a normal value — it means "no surface", not "not recorded".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
```

And change `record`'s signature and its `wanted` binding:

```rust
pub fn record(
    socket: &Path,
    session: &str,
    workspace: &Path,
    env: BTreeMap<String, String>,
) -> std::io::Result<()> {
    let wanted = SessionRef {
        session: session.to_string(),
        workspace: workspace.to_path_buf(),
        env,
    };
```

The rest of `record` is unchanged: it still compares against the existing file
and skips the write when nothing changed.

- [ ] **Step 8: Update the hook**

In `crates/thirdeye/src/cmd_hook.rs`, replace lines 95–98 with:

```rust
        // Captured here because this is the only process that has it: the
        // daemon inherits systemd's environment, not the agent's. What is
        // worth capturing is the surfaces' business, not the hook's — see
        // `surface::SURFACE_ENV`.
        let env = crate::daemon::surface::capture(|key| std::env::var(key).ok());
        let _ = crate::session_ref::record(&socket, session, paths.workspace().root(), env);
```

The comment on the line above (`// Recorded before the poke: ...`) stays.

- [ ] **Step 9: Update the daemon's kitty lookup**

In `crates/thirdeye/src/daemon/mod.rs`, change `kitty_surface` to read the map.
This is temporary — Task 5 deletes the function entirely — but the tree must
build and pass at the end of every task:

```rust
fn kitty_surface(socket: &std::path::Path, session: &str) -> Option<KittySurface<ProcessRunner>> {
    let listen_on = session_ref::load(socket)?.env.get("KITTY_LISTEN_ON")?.clone();
    let surface = KittySurface::new(listen_on, session, ProcessRunner);
    surface.available().then_some(surface)
}
```

- [ ] **Step 10: Run the gates**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 11: Commit**

```bash
git add crates/thirdeye/src/daemon/surface/mod.rs \
        crates/thirdeye/src/session_ref.rs \
        crates/thirdeye/src/cmd_hook.rs \
        crates/thirdeye/src/daemon/mod.rs
git commit -m "feat(surface): record the environment surfaces declare, not the one kitty needs"
```

---

## Task 3: The tmux surface

**Files:**
- Create: `crates/thirdeye/src/daemon/surface/tmux.rs`
- Modify: `crates/thirdeye/src/daemon/surface/mod.rs` (add `pub mod tmux;`)

**Interfaces:**
- Consumes: `runner::Runner` (Task 1), `SURFACE_ENV` keys `TMUX`/`TMUX_PANE`
  (Task 2), `Surface` and `ViewRequest` (existing).
- Produces:
  - `tmux::detects(env: &BTreeMap<String, String>) -> bool`
  - `tmux::TmuxSurface::<R>::from_env(env: &BTreeMap<String, String>, session: &str, runner: R) -> Option<Self>`
  - `impl Surface for TmuxSurface<R>`

- [ ] **Step 1: Write the failing tests**

Create `crates/thirdeye/src/daemon/surface/tmux.rs` with only the test module,
so the behaviour is named before the type exists:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default, Clone)]
    struct Recorder {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        stdout: String,
        available: bool,
        fail: bool,
    }

    impl Recorder {
        fn working() -> Self {
            Self {
                stdout: "%7".into(),
                available: true,
                ..Default::default()
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for Recorder {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().cloned());
            self.calls.lock().unwrap().push(call);
            if self.fail {
                bail!("planted failure");
            }
            Ok(self.stdout.clone())
        }
        fn available(&self, _program: &str) -> bool {
            self.available
        }
    }

    fn env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("TMUX".into(), "/tmp/tmux-1000/default,4242,0".into());
        env.insert("TMUX_PANE".into(), "%3".into());
        env
    }

    fn request() -> ViewRequest {
        ViewRequest {
            session: "s1".into(),
            workspace: "/home/u/app".into(),
        }
    }

    fn surface(runner: Recorder) -> TmuxSurface<Recorder> {
        TmuxSurface::from_env(&env(), "s1", runner).unwrap()
    }

    #[test]
    fn the_server_is_named_by_the_socket_inside_tmux() {
        // The daemon shares none of the agent's environment, so there is no
        // ambient server for `tmux` to find. $TMUX is
        // `<socket>,<pid>,<index>`; only the first field addresses a server.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        let joined = tmux.runner.calls()[0].join(" ");
        assert!(
            joined.starts_with("tmux -S /tmp/tmux-1000/default split-window"),
            "{joined}"
        );
    }

    #[test]
    fn the_split_lands_beside_the_agents_own_pane() {
        // Splitting the server's "current" pane would put the head next to
        // whatever the human last touched, which on a busy server is not the
        // agent at all.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        let joined = tmux.runner.calls()[0].join(" ");
        assert!(joined.contains("-t %3"), "{joined}");
        assert!(joined.contains("-c /home/u/app"), "{joined}");
    }

    #[test]
    fn the_split_never_steals_focus() {
        // The human is typing at the agent. A viewer that grabs the cursor
        // mid-sentence is worse than no viewer at all. `-d` is the whole
        // reason tmux is preferred over Zellij.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        assert!(tmux.runner.calls()[0].iter().any(|a| a == "-d"));
    }

    #[test]
    fn closing_targets_the_pane_id_tmux_gave_us() {
        // Not a title, which a human may rename, and not a position, which
        // moves as other panes come and go.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        tmux.close().unwrap();
        let joined = tmux.runner.calls()[1].join(" ");
        assert_eq!(
            joined,
            "tmux -S /tmp/tmux-1000/default kill-pane -t %7"
        );
    }

    #[test]
    fn a_session_id_cannot_smuggle_a_second_command_into_the_shell() {
        // tmux runs shell-command through sh, and session ids are agent
        // supplied — the same reason every path goes through
        // `sanitize_component`. Planted defect: drop the quoting and this
        // argv ends with a live `rm -rf ~`.
        let hostile = "s1'; rm -rf ~; echo '";
        let mut env = env();
        let mut tmux = TmuxSurface::from_env(&env, hostile, Recorder::working()).unwrap();
        env.clear();
        tmux.open(&request()).unwrap();

        let command = tmux.runner.calls()[0].last().unwrap().clone();
        assert!(
            command.starts_with("thirdeye head --session '"),
            "{command}"
        );
        assert!(
            !command.contains("; rm -rf ~; echo"),
            "an unquoted session id would run a second command: {command}"
        );
    }

    #[test]
    fn opening_twice_splits_once() {
        // The visibility rules ask on every event, so open is called far more
        // often than it transitions.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        tmux.open(&request()).unwrap();
        assert_eq!(tmux.runner.calls().len(), 1);
    }

    #[test]
    fn a_split_that_prints_no_pane_id_leaves_the_surface_closed() {
        // Without an id there is no handle to close, and believing the pane
        // exists would leave the human with a viewer nothing can tear down.
        let mut tmux = surface(Recorder {
            stdout: String::new(),
            available: true,
            ..Default::default()
        });
        assert!(tmux.open(&request()).is_err());
        assert!(!tmux.is_open());
    }

    #[test]
    fn a_pane_the_human_already_closed_does_not_trap_the_surface() {
        // kill-pane fails when the pane is gone. Refusing to forget it would
        // mean retrying forever and never reopening.
        let recorder = Recorder::working();
        let mut tmux = surface(recorder.clone());
        tmux.open(&request()).unwrap();

        tmux.runner = Recorder {
            fail: true,
            available: true,
            calls: recorder.calls.clone(),
            ..Default::default()
        };
        assert!(tmux.close().is_err());
        assert!(!tmux.is_open(), "a failed close must not look open");
    }

    #[test]
    fn an_agent_started_outside_tmux_has_no_tmux_surface() {
        // §12: absence is the signal, not an error to report.
        assert!(!detects(&BTreeMap::new()));
        assert!(TmuxSurface::from_env(&BTreeMap::new(), "s1", Recorder::working()).is_none());
    }

    #[test]
    fn tmux_without_a_pane_is_not_a_surface() {
        // $TMUX without $TMUX_PANE means the variable was inherited by a
        // process that is not itself in a pane. Splitting on a guess would
        // put the head in someone else's window.
        let mut env = BTreeMap::new();
        env.insert("TMUX".into(), "/tmp/tmux-1000/default,4242,0".into());
        assert!(!detects(&env));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p thirdeye tmux 2>&1 | tail -20`
Expected: compile error — module `tmux` is not declared and `TmuxSurface` does
not exist.

- [ ] **Step 3: Write the implementation**

Prepend to `crates/thirdeye/src/daemon/surface/tmux.rs`:

```rust
//! Showing the head in a tmux pane.
//!
//! The surface with the widest reach: tmux is a substrate inside *any*
//! emulator, so one implementation covers the terminals that expose no
//! control interface of their own — and it is the only surface that survives
//! SSH, where a local emulator's control socket is unreachable from the host
//! the agent is actually running on.
//!
//! Everything here is one `tmux` invocation or another, which is also the
//! whole risk: the commands can only be *proved* against a running server. So
//! the spawning is injected, the tests below assert the exact argv, and a
//! human runs the real thing (see `docs/tmux.md`).

use std::collections::BTreeMap;
use std::fmt::Debug;

use anyhow::{Result, bail};

use super::runner::Runner;
use super::{Surface, ViewRequest};

/// Whether the recorded environment describes a tmux pane.
///
/// Separate from construction so the registry can order surfaces without
/// building any of them, and so `mod.rs` never needs to know what a tmux
/// socket looks like.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    address(env).is_some()
}

/// `(server socket, the agent's pane)`, if both are recorded.
///
/// `$TMUX` is `<socket-path>,<server-pid>,<session-index>`; only the first
/// field addresses a server. `$TMUX_PANE` is required as well: `$TMUX` alone
/// can be inherited by a process that is not itself in a pane, and splitting
/// on a guess would put the head in someone else's window.
fn address(env: &BTreeMap<String, String>) -> Option<(String, String)> {
    let socket = env.get("TMUX")?.split(',').next()?.to_string();
    let pane = env.get("TMUX_PANE")?.clone();
    (!socket.is_empty() && !pane.is_empty()).then_some((socket, pane))
}

#[derive(Debug)]
pub struct TmuxSurface<R> {
    socket: String,
    /// The agent's own pane — what we split, and never the server's notion of
    /// the current one.
    pane: String,
    session: String,
    runner: R,
    /// The pane we made, as tmux named it (`%7`). The only precise handle for
    /// closing it: titles are renameable and positions move.
    opened: Option<String>,
}

impl<R: Runner> TmuxSurface<R> {
    pub fn from_env(env: &BTreeMap<String, String>, session: &str, runner: R) -> Option<Self> {
        let (socket, pane) = address(env)?;
        Some(Self {
            socket,
            pane,
            session: session.to_string(),
            runner,
            opened: None,
        })
    }

    fn split_args(&self, request: &ViewRequest) -> Vec<String> {
        vec![
            // Explicit rather than inherited: the daemon is socket-activated
            // and has none of the agent's environment, so there is no ambient
            // server for `tmux` to find.
            "-S".into(),
            self.socket.clone(),
            "split-window".into(),
            // Detached — the new pane is created without becoming active.
            "-d".into(),
            // Print the new pane's id. Nothing else ever tells us what it is.
            "-P".into(),
            "-F".into(),
            "#{pane_id}".into(),
            // Beside, not below: §10.1 is a side-by-side layout.
            "-h".into(),
            "-l".into(),
            "40%".into(),
            "-t".into(),
            self.pane.clone(),
            "-c".into(),
            request.workspace.display().to_string(),
            head_command(&self.session),
        ]
    }

    fn kill_args(&self, pane: &str) -> Vec<String> {
        vec![
            "-S".into(),
            self.socket.clone(),
            "kill-pane".into(),
            "-t".into(),
            pane.to_string(),
        ]
    }
}

/// The command the pane runs.
///
/// tmux hands `shell-command` to `sh`, and session ids are agent-supplied —
/// the same untrusted input every path already routes through
/// `sanitize_component`. Quoting it is what stops a session id containing `;`
/// from running whatever follows.
fn head_command(session: &str) -> String {
    format!("thirdeye head --session {}", single_quote(session))
}

fn single_quote(s: &str) -> String {
    // The POSIX idiom: end the quoted run, emit an escaped quote, start a new
    // run. There is no escape *inside* single quotes to use instead.
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl<R: Runner + Debug> Surface for TmuxSurface<R> {
    fn id(&self) -> &'static str {
        "tmux"
    }

    fn available(&self) -> bool {
        self.runner.available("tmux")
    }

    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        if self.opened.is_some() {
            return Ok(());
        }
        let pane = self.runner.run("tmux", &self.split_args(request))?;
        let pane = pane.trim().to_string();
        if pane.is_empty() {
            // No id means no handle to close with. Reporting success here
            // would leave a pane nothing can ever tear down.
            bail!("tmux split-window printed no pane id");
        }
        self.opened = Some(pane);
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        let Some(pane) = self.opened.take() else {
            return Ok(());
        };
        // Taken before the call succeeds: `kill-pane` fails when the human
        // already closed it, and retrying forever is worse than believing it
        // is gone — the next open simply splits again. Same rule as kitty.
        self.runner.run("tmux", &self.kill_args(&pane)).map(|_| ())
    }

    fn is_open(&self) -> bool {
        // The stored id, not a `list-panes` call: `is_open` is asked on every
        // refresh, and a process spawn per event is a cost the hot path must
        // not carry. A pane the human closed is discovered at the next
        // `close`, which is exactly when it matters.
        self.opened.is_some()
    }
}
```

Note: `Surface::id` does not exist yet — Task 4 adds it. Implement it here
anyway; the tree will not compile until Step 4 below.

- [ ] **Step 4: Add `id` to the trait and the other surfaces**

In `crates/thirdeye/src/daemon/surface/mod.rs`, add to the `Surface` trait,
above `available`:

```rust
    /// Stable name, as `THIRDEYE_SURFACE` spells it and as `doctor` reports
    /// it. A surface the user cannot name is one they cannot override.
    fn id(&self) -> &'static str;
```

Also widen the supertrait bound so a resolved surface can be boxed and moved
onto the serving thread in Task 5:

```rust
pub trait Surface: Debug + Send {
```

In `kitty.rs`, add to `impl Surface for KittySurface<R>`:

```rust
    fn id(&self) -> &'static str {
        "kitty"
    }
```

In `null.rs`, add to `impl Surface for NullSurface`:

```rust
    fn id(&self) -> &'static str {
        "none"
    }
```

And register the module in `mod.rs`:

```rust
pub mod kitty;
pub mod null;
pub mod runner;
pub mod tmux;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p thirdeye tmux 2>&1 | tail -30`
Expected: PASS, all ten tests.

- [ ] **Step 6: Perturbation-test the quoting**

The security test must be shown to go red on the defect it guards, per the
project rule. Temporarily change `head_command` to interpolate raw:

```rust
    format!("thirdeye head --session {session}")
```

Run: `cargo test -p thirdeye a_session_id_cannot_smuggle 2>&1 | tail -20`
Expected: FAIL, with the message "an unquoted session id would run a second
command".

Then restore `single_quote(session)` and re-run:
Expected: PASS.

- [ ] **Step 7: Run the gates**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: PASS.

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 8: Commit**

```bash
git add crates/thirdeye/src/daemon/surface/tmux.rs \
        crates/thirdeye/src/daemon/surface/mod.rs \
        crates/thirdeye/src/daemon/surface/kitty.rs \
        crates/thirdeye/src/daemon/surface/null.rs
git commit -m "feat(surface): a tmux pane, which reaches every emulator and survives ssh"
```

---

## Task 4: The registry and the resolution rule

**Files:**
- Modify: `crates/thirdeye/src/daemon/surface/mod.rs` (add `ORDER`, `detected`, `resolve`, `impl Surface for Box<dyn Surface>`)

**Interfaces:**
- Consumes: `tmux::detects`, `kitty::detects`, `Surface::id` (Task 3).
- Produces:
  - `surface::detected(env: &BTreeMap<String, String>) -> Vec<&'static str>`
  - `surface::resolve(env: &BTreeMap<String, String>, session: &str) -> Box<dyn Surface>`

- [ ] **Step 1: Add `detects` to kitty**

kitty has no `detects` yet. In `crates/thirdeye/src/daemon/surface/kitty.rs`,
add above the struct:

```rust
/// Whether the recorded environment names a kitty instance to drive.
///
/// §12 makes the absence of the address exactly the "kitty is not available"
/// signal — a normal state, not an error to report.
pub fn detects(env: &std::collections::BTreeMap<String, String>) -> bool {
    env.get("KITTY_LISTEN_ON").is_some_and(|v| !v.is_empty())
}
```

And a constructor that reads the map, mirroring tmux's:

```rust
impl<R: Runner> KittySurface<R> {
    pub fn from_env(
        env: &std::collections::BTreeMap<String, String>,
        session: &str,
        runner: R,
    ) -> Option<Self> {
        let listen_on = env.get("KITTY_LISTEN_ON")?;
        (!listen_on.is_empty()).then(|| Self::new(listen_on.clone(), session, runner))
    }
}
```

Add this as a second `impl` block rather than editing the existing one, so the
diff stays reviewable.

- [ ] **Step 2: Write the failing tests**

Append to the `#[cfg(test)] mod tests` in
`crates/thirdeye/src/daemon/surface/mod.rs`:

```rust
    fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn a_multiplexer_beats_the_emulator_hosting_it() {
        // An agent in tmux inside kitty must get a tmux pane. A kitty window
        // would open *outside* the tmux client the human is looking at, and
        // over SSH kitty's socket is not reachable from the host the agent is
        // running on at all — while tmux's is.
        let env = env_of(&[
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
            ("KITTY_LISTEN_ON", "unix:/tmp/kitty-4242"),
        ]);
        assert_eq!(detected(&env), vec!["tmux", "kitty"]);
    }

    #[test]
    fn an_agent_in_no_known_container_detects_nothing() {
        assert!(detected(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn an_explicit_choice_outranks_detection() {
        // Someone who sets THIRDEYE_SURFACE is saying they know better, which
        // is the only reason to type it.
        let env = env_of(&[
            ("THIRDEYE_SURFACE", "kitty"),
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
            ("KITTY_LISTEN_ON", "unix:/tmp/kitty-4242"),
        ]);
        assert_eq!(chosen(&env), Some("kitty"));
    }

    #[test]
    fn naming_a_surface_that_is_not_there_opens_nothing() {
        // Falling back to the detected surface would silently ignore the
        // override, and the human would be debugging the wrong thing.
        let env = env_of(&[
            ("THIRDEYE_SURFACE", "kitty"),
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
        ]);
        assert_eq!(chosen(&env), None);
    }

    #[test]
    fn a_human_can_turn_the_surface_off_from_inside_one() {
        let env = env_of(&[
            ("THIRDEYE_SURFACE", "none"),
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
        ]);
        assert_eq!(chosen(&env), None);
    }

    #[test]
    fn every_registered_surface_has_a_distinct_id() {
        // The id is what THIRDEYE_SURFACE matches on; a collision would make
        // one surface silently answer for another.
        let mut ids: Vec<&str> = ORDER.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p thirdeye surface::tests 2>&1 | tail -20`
Expected: compile error — `detected`, `chosen` and `ORDER` not found.

- [ ] **Step 4: Write the implementation**

Add to `crates/thirdeye/src/daemon/surface/mod.rs`:

```rust
/// Every surface this build knows about, **innermost first**.
///
/// Adding a surface is adding a file and a line here. That is the whole point
/// of the seam: no existing implementation is edited, and nothing else in the
/// daemon learns a new name.
pub const ORDER: &[(&str, fn(&BTreeMap<String, String>) -> bool)] = &[
    ("tmux", tmux::detects),
    ("kitty", kitty::detects),
];

/// The ids of every surface the recorded environment could support, innermost
/// first.
///
/// Split out from construction so the ordering rule — the part that is
/// actually easy to get wrong — is testable without spawning a process or
/// owning a terminal.
pub fn detected(env: &BTreeMap<String, String>) -> Vec<&'static str> {
    ORDER
        .iter()
        .filter(|(_, detects)| detects(env))
        .map(|(id, _)| *id)
        .collect()
}

/// Which surface to use, or `None` for no surface at all.
///
/// **Most-nested wins.** An agent running in tmux inside kitty gets a tmux
/// pane: the head must appear in the container the human is actually looking
/// at, and a kitty window would open beside the tmux client rather than
/// inside it.
///
/// An explicit `THIRDEYE_SURFACE` outranks detection entirely, including
/// `none`. When it names a surface that is not present the answer is `None`
/// rather than the detected one — silently ignoring the override would leave
/// the human debugging the wrong thing.
pub fn chosen(env: &BTreeMap<String, String>) -> Option<&'static str> {
    let available = detected(env);
    match env.get("THIRDEYE_SURFACE").map(String::as_str) {
        Some("none") => None,
        Some(named) => available.into_iter().find(|id| *id == named),
        None => available.into_iter().next(),
    }
}

/// The surface for this session.
///
/// `NullSurface` when nothing matches — a daemon with no surface still tails,
/// groups and serves heads (§12). It simply never opens one unasked.
pub fn resolve(env: &BTreeMap<String, String>, session: &str) -> Box<dyn Surface> {
    let fallback: Box<dyn Surface> = Box::new(NullSurface::default());
    let Some(id) = chosen(env) else {
        return fallback;
    };

    let built: Option<Box<dyn Surface>> = match id {
        "tmux" => tmux::TmuxSurface::from_env(env, session, runner::ProcessRunner)
            .map(|s| Box::new(s) as Box<dyn Surface>),
        "kitty" => kitty::KittySurface::from_env(env, session, runner::ProcessRunner)
            .map(|s| Box::new(s) as Box<dyn Surface>),
        _ => None,
    };

    // `detects` said the environment describes this surface; `available` asks
    // whether the program is installed, which is a different question and can
    // still be no.
    match built {
        Some(s) if s.available() => s,
        _ => fallback,
    }
}

/// So a resolved surface can be held as a trait object without the daemon
/// becoming generic over a box.
impl Surface for Box<dyn Surface> {
    fn id(&self) -> &'static str {
        (**self).id()
    }
    fn available(&self) -> bool {
        (**self).available()
    }
    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        (**self).open(request)
    }
    fn close(&mut self) -> Result<()> {
        (**self).close()
    }
    fn is_open(&self) -> bool {
        (**self).is_open()
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p thirdeye surface::tests 2>&1 | tail -30`
Expected: PASS.

- [ ] **Step 6: Run the gates**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS, clean.

- [ ] **Step 7: Commit**

```bash
git add crates/thirdeye/src/daemon/surface/mod.rs \
        crates/thirdeye/src/daemon/surface/kitty.rs
git commit -m "feat(surface): a registry, and the rule that the innermost container wins"
```

---

## Task 5: The daemon resolves instead of hardcoding kitty

**Files:**
- Modify: `crates/thirdeye/src/daemon/mod.rs:36-40` (imports), `:66-72`
  (one `session_ref::load`), `:94-100` (resolve), `:384-393` (delete
  `kitty_surface`)

**Interfaces:**
- Consumes: `surface::resolve` (Task 4).
- Produces: nothing new; `Daemon<Box<dyn Surface>>` is now the served type.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `crates/thirdeye/src/daemon/mod.rs`:

```rust
    #[test]
    fn a_daemon_can_be_built_over_a_resolved_surface() {
        // The daemon must not be generic over a concrete surface type, or
        // adding one would mean editing `run`. Resolving to a trait object is
        // what keeps the registry the only place that names a surface.
        let env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
        let surface = crate::daemon::surface::resolve(&env, SESSION);
        assert_eq!(surface.id(), "none");

        let harness = Harness::new("resolved");
        let daemon = Daemon::new(SESSION, harness.paths(), surface);
        assert_eq!(daemon.surface.id(), "none");
    }
```

If the existing test harness type is not named `Harness` or its constructor
differs, match the surrounding tests in that module — they already build a
`Daemon<NullSurface>` via `self.daemon()` around line 518, and this test needs
only the same paths.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p thirdeye a_daemon_can_be_built_over_a_resolved 2>&1 | tail -20`
Expected: FAIL — `resolve` is not reachable, or `Daemon::new` rejects
`Box<dyn Surface>`.

- [ ] **Step 3: Load the pointer once**

Replace `crates/thirdeye/src/daemon/mod.rs:66-72` with:

```rust
    let recorded = session_ref::load(&socket);
    let (session, paths) = match &recorded {
        Some(found) if explicit_workspace.is_none() => (
            found.session.clone(),
            EventPaths::discover(Workspace::new(found.workspace.clone())),
        ),
        _ => (session, paths),
    };
```

The comment above it stays as written — it still describes exactly this.

- [ ] **Step 4: Resolve the surface**

Replace `crates/thirdeye/src/daemon/mod.rs:94-100` with:

```rust
    let env = recorded.map(|r| r.env).unwrap_or_default();
    let surface = surface::resolve(&env, &session);
    let result = serve(Daemon::new(&session, paths, surface), listener);
```

Rewrite the comment above it, which currently names kitty specifically:

```rust
    // Which surface is a fact about the world, not a setting: the hook
    // recorded what container the agent was started in, and §12 makes the
    // absence of any the "no surface" signal rather than an error. A daemon
    // with no surface still tails, groups and serves heads — it simply never
    // opens one unasked.
```

- [ ] **Step 5: Delete the hardcoded constructor**

Delete `fn kitty_surface(...)` and its doc comment entirely
(`crates/thirdeye/src/daemon/mod.rs:384-393`). Remove the now-unused imports at
lines 40–41:

```rust
use surface::kitty::KittySurface;
use surface::runner::ProcessRunner;
```

Keep `use surface::{NullSurface, Surface, ViewRequest};` — the tests still use
`NullSurface`.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p thirdeye daemon 2>&1 | tail -30`
Expected: PASS. Every pre-existing daemon test still builds its own
`Daemon<NullSurface>` and is unaffected.

- [ ] **Step 7: Run the gates**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS, clean.

- [ ] **Step 8: Commit**

```bash
git add crates/thirdeye/src/daemon/mod.rs
git commit -m "refactor(daemon): resolve a surface instead of naming one"
```

---

## Task 6: Doctor reports the surface, and the docs say how to check it

**Files:**
- Modify: `crates/thirdeye/src/cmd_doctor.rs`
- Create: `docs/tmux.md`
- Modify: `CLAUDE.md`

**Interfaces:**
- Consumes: `surface::capture`, `surface::detected`, `surface::chosen`.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Write the failing test**

Add to `crates/thirdeye/src/cmd_doctor.rs`'s test module:

```rust
    #[test]
    fn the_surface_line_names_what_was_detected_and_what_won() {
        // The milestone 7 failure this command exists for, in its next form:
        // every component correct, no surface, and nothing saying so. A user
        // in tmux inside kitty needs to see *both* were found and which one
        // was chosen, or "why did it open in the wrong place" has no answer.
        let env = [
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
            ("KITTY_LISTEN_ON", "unix:/tmp/kitty-4242"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();

        let line = surface_report(&env);
        assert!(line.contains("tmux"), "{line}");
        assert!(line.contains("kitty"), "{line}");
        assert!(line.contains("would use tmux"), "{line}");
    }

    #[test]
    fn no_surface_is_reported_as_a_state_not_a_problem() {
        // §12: an agent outside any known container is normal. Printing it as
        // an error would send someone hunting a bug that is not there.
        let line = surface_report(&std::collections::BTreeMap::new());
        assert!(line.contains("no surface"), "{line}");
        assert!(!line.to_lowercase().contains("error"), "{line}");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p thirdeye doctor 2>&1 | tail -20`
Expected: compile error — `surface_report` not found.

- [ ] **Step 3: Implement the report**

Add to `crates/thirdeye/src/cmd_doctor.rs`:

```rust
/// One line describing what surface this environment would get.
///
/// Takes the captured map rather than reading the environment, both because
/// `std::env::set_var` is unsafe in Rust 2024 and because this must report on
/// the same allowlist the hook records — not on a second, drifting idea of
/// what matters.
pub fn surface_report(env: &std::collections::BTreeMap<String, String>) -> String {
    let found = crate::daemon::surface::detected(env);
    match crate::daemon::surface::chosen(env) {
        Some(id) => format!("surface: detected [{}], would use {id}", found.join(", ")),
        None if found.is_empty() => {
            "surface: no surface (not running inside tmux or kitty)".to_string()
        }
        None => format!(
            "surface: detected [{}], but THIRDEYE_SURFACE selects none of them",
            found.join(", ")
        ),
    }
}
```

Call it from the command's main body, beside the other checks, passing the real
environment:

```rust
    println!(
        "{}",
        surface_report(&crate::daemon::surface::capture(|k| std::env::var(k).ok()))
    );
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p thirdeye doctor 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Write the manual verification doc**

Create `docs/tmux.md`:

```markdown
# Verifying the tmux surface

The surface is the one part of thirdeye that cannot be tested in CI: the argv
is asserted in `crates/thirdeye/src/daemon/surface/tmux.rs`, but only a running
server proves the arguments are the right ones. This is the procedure, and it
is the same shape as `docs/kitty.md`.

Verified against tmux __VERSION__ on __DATE__.

## Setup

    cargo xtask install     # binary, plugin and units — never just one

Start a tmux session and run the agent inside it:

    tmux new -s thirdeye-check
    claude

`echo $TMUX $TMUX_PANE` inside that pane must print a socket path and a pane
id. If either is empty the hook records nothing and the daemon correctly
concludes "no surface" — check that before suspecting the surface code.

## What to check

1. **`thirdeye doctor` reports the surface.** It must say `would use tmux`.
   Inside tmux *inside kitty* it must list both and still choose tmux.
2. **A mutation opens a pane beside the agent** (§10.2: a mutation or a flag,
   never a read alone). Ask the agent to edit a file.
3. **Focus does not move.** Keep typing at the agent as the pane opens. Every
   keystroke must land in the agent's pane. This is the requirement `-d`
   exists for and the reason tmux is preferred over Zellij.
4. **The pane runs the head**, at the workspace root, showing the current turn.
5. **`/thirdeye-close` closes that pane and no other.** Split the window
   manually first so there is a bystander pane to get this wrong with.
6. **`SessionEnd` takes the pane with it.** Exit the agent; the pane closes.
7. **A pane the human closed does not wedge the surface.** Close it by hand,
   then trigger another mutation. A new pane must open.

## Known gaps

- `is_open` trusts the id it stored rather than asking `list-panes`, so a pane
  closed by hand is discovered at the next `close`, not immediately. That is
  deliberate: the check is on the hot path, and a process spawn per event
  costs more than it buys.
- The split is `-h -l 40%` unconditionally. A narrow terminal gets a narrow
  head.
```

Fill `__VERSION__` from `tmux -V` and `__DATE__` with the date of the run.

- [ ] **Step 6: Update CLAUDE.md**

In the seam table, change the Surface row's "Today" column to:

```
| Surface | `crates/thirdeye/src/daemon/surface/` | `tmux.rs`, `kitty.rs`, `null.rs` | `wezterm.rs` |
```

Add to the **Gotchas** section:

```markdown
- **A surface is chosen by nesting, not by preference.** `surface::ORDER` is
  innermost-first, so an agent in tmux inside kitty gets a tmux pane — a kitty
  window would open beside the tmux client rather than inside it, and over SSH
  kitty's control socket is not reachable from the host running the agent at
  all. `THIRDEYE_SURFACE` overrides it, including `none`, and naming a surface
  that is not present yields *no* surface rather than the detected one.
- **What the hook captures from the agent's environment is declared in
  `surface::SURFACE_ENV`, not in the hook.** Adding a surface that needs a new
  variable means adding it to that list; the hook copies whatever is there and
  knows nothing about terminals. It is an allowlist rather than the whole
  environment because the file lands beside a log that already holds prompt
  text — the agent's full environment would put credentials there too.
- **Nothing may call `std::env::set_var`.** It is `unsafe` in Rust 2024 and the
  workspace forbids `unsafe`. Anything whose behaviour depends on the
  environment takes a lookup closure instead, which is why `surface::capture`
  has the signature it does.
```

Under the **Built — the kitty surface** section, add a sibling paragraph:

```markdown
Built — the **surface registry and tmux surface**: `surface::ORDER` plus
`resolve`, and `daemon/surface/tmux.rs`. tmux reaches every terminal emulator
rather than one, and is the only surface that survives SSH — a local
emulator's control socket is unreachable from the host the agent runs on.
`split-window -d` creates the pane without taking focus, `-P -F '#{pane_id}'`
yields the only precise handle for closing it, and the session id is
shell-quoted because tmux hands the command to `sh` and session ids are
agent-supplied. Verified live: `docs/tmux.md`.
```

- [ ] **Step 7: Run the gates**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS, clean.

- [ ] **Step 8: Verify against a real tmux server**

Run: `cargo xtask install`

Then work through `docs/tmux.md` end to end, filling in the version and date.
**This step is not optional and cannot be delegated to the test suite** — every
argv in `tmux.rs` is asserted against a string the author wrote, and only a
real server proves the string was right.

- [ ] **Step 9: Commit**

```bash
git add crates/thirdeye/src/cmd_doctor.rs docs/tmux.md CLAUDE.md
git commit -m "feat(doctor): say which surface this environment would get"
```

---

## Self-Review Notes

Checked against the design discussion that produced this plan:

- **Surface hint capture generalized** — Task 2. `SessionRef.env` is a map; the
  allowlist is declared by the surface layer.
- **Surface registry with most-nested-wins** — Task 4, with the SSH reasoning
  recorded in the doc comment and pinned by
  `a_multiplexer_beats_the_emulator_hosting_it`.
- **tmux surface** — Task 3, including the shell-injection guard the design
  discussion did not anticipate but that tmux's `sh`-based `shell-command`
  forces.
- **Runner returning stdout** — Task 1; discovered while planning Task 3, since
  `-P` printing the pane id is the only way to address it.
- **`Surface::id`** — added in Task 3 because `THIRDEYE_SURFACE` needs a name to
  match on and `doctor` needs one to print.

**Deliberately not in this plan**, each its own plan:

1. **Adapter dispatch (`thirdeye hook --agent <id>`).** It belongs with the
   Codex adapter, where a second adapter makes it testable. Dispatch with one
   adapter is a branch that cannot be exercised.
2. **Portable activation (self-spawn + file lock).** This is what makes macOS
   work at all — the daemon is reachable only through `systemctl --user`
   today. Lifecycle, not seams.
3. **Codex adapter**, gated on a live capture spike producing
   `docs/codex-hook-payload.md`. The published research cites a feature-request
   discussion for Codex's hooks; the shape is unverified and the project rule
   is that payloads are captured, never inferred.
4. **OpenCode plugin** — one dependency-free file, `include_str!`'d and written
   out by the installer, exactly as the units already are.

A browser head was item 4 here; it was dropped from the roadmap on
2026-10-08. The protocol still stays newline-delimited JSON, so a head in
another language remains possible — it is just not planned.

**For the spec**, since `docs/design/specs/` is not edited here:
§5's Surface row moves tmux from Deferred to PoC; §12 becomes one section per
surface, or gains a sibling; and the resolution rule (most-nested wins,
`THIRDEYE_SURFACE` overriding, a named-but-absent surface yielding none) is new
spec text with no home yet.

---

## Execution notes (2026-10-08)

Done, and verified live against tmux 3.4 (`docs/tmux.md`). Where execution
departed from the plan:

- **Tasks 3–5 landed as one commit.** thirdeye is a binary crate, so the tmux
  surface and the registry are dead code until the daemon calls them, and
  each task alone fails `clippy -D warnings`.
- **The injection test in Task 3 was wrong as written.** It asserted the
  command did not *contain* the payload text, which survives correct quoting
  too — so it failed the correct code. It now runs the command through a real
  `sh` and checks what came out; perturbed against raw interpolation and
  naive quoting.
- **`resolve` is split into `resolve` and `build`**, so a test can check every
  registered id against the id its surface reports.
- **Doctor's surface line is a `Note` finding**, not a bare `println!`.
- **Check 7 of `docs/tmux.md` contradicted the plan's own known gap** and
  failed live: a pane closed by hand was never reopened, by anything. Neither
  was `SessionEnd` closing the pane — SIGTERM skips the daemon's teardown.
  Both were pre-existing and true of kitty. Fixing them grew into hide
  conditions chosen by the human (`next-mutation`, `next-turn`, `session`),
  the head exiting when its daemon does, and the MCP server poking the daemon
  after a flag. CLAUDE.md describes the result.

Still for the spec, unchanged from above: §5's Surface row, §12, and the
resolution rule have no spec text yet — and neither do hide conditions.
