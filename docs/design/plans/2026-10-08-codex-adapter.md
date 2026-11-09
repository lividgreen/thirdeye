# Codex Adapter Implementation Plan

**Goal:** thirdeye works with OpenAI Codex CLI as it does with Claude Code —
turns, the files Codex changed, flags, and the viewer in tmux or kitty — and
every agent's shell commands report the files they wrote.

**Architecture:** A second adapter, `codex.rs`, chosen by `thirdeye hook
--agent codex`. Codex changes files through arbitrary shell commands, so the
adapter does not try to read mutations out of payloads: it brackets every tool
call (`PreToolUse` → `PostToolUse`, paired by `tool_use_id`) and the hook
reports the workspace files whose mtime falls inside that window. Codex gives
MCP servers no session id, so `flag` falls back to the workspace's most
recently active session, which every hook records. Codex is wired by writing
its project config (`.codex/hooks.json`, `.codex/config.toml`,
`.codex/skills/`) — a `thirdeye codex install` subcommand, as `thirdeye units`
already does for systemd.

**Tech Stack:** Rust 2024, `serde_json`, `std::fs`/`std::process`; `git` at
run time when the workspace is a repository. No new crates.

**Spec:** there is no spec section for Codex yet (plans do not edit
`docs/design/specs/`). This plan argues from the captured facts in
`docs/codex-hook-payload.md` and two decisions made on 2026-10-08: mutations
are found by **watching the filesystem**, and a flag finds its session as
**the latest session in its workspace**.

## Global Constraints

- Payload facts come only from `docs/codex-hook-payload.md`. Nothing about
  `apply_patch` is assumed — it was never captured.
- `tool_response` is never stored: for Codex it is a command's output, which
  for `cat` is a file's content.
- `thirdeye hook` with no `--agent` must behave exactly as today: every
  installed Claude plugin calls it that way.
- Codex `turn_id` is stored where Claude's `prompt_id` goes
  (`FileEvent::prompt_id`, `TurnMarker` id). No core type changes.
- Observed changes are `EventKind::Write` with `Source::Hook`. The window says
  a file was written, not how; claiming `Edit` with a range would be invented.
- An observed event's `dedup_key` is `"{tool_use_id}:{absolute path}"`.
- `PreToolUse` is synchronous (it must land before the command runs); every
  other Codex hook is `"async": true`, except `SessionEnd`, which Codex always
  runs synchronously — give it `"timeout": 3`, the maximum.
- `std::env::set_var` is unavailable (`unsafe` is forbidden); anything
  reading the environment takes a lookup closure.
- Every task ends green on `cargo test --workspace`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo fmt --check`. thirdeye is
  a binary crate: code with no caller fails clippy, so each task wires what it
  adds.
- Every new indicator gets a perturbation test (CLAUDE.md, Conventions).

## Review Focus

1. **A human saves a file while Codex's command runs** — it is reported as the
   agent's write. Expected and documented, not fixed; but a file whose mtime
   is *before* the marker must never be reported (Task 3 test).
2. **A large repository** — `node_modules`, `target/`, anything git-ignored
   must be neither walked nor reported (Task 3 test).
3. **A workspace that is not a git repository** — the fallback walk skips
   hidden directories and stops at a cap rather than crawling a home
   directory for every tool call (Task 3 test).
4. **A command interrupted before `PostToolUse`, or hooks installed in the
   middle of one** — a missing marker yields no events and no error, and
   `SessionEnd` removes stale markers (Task 3 tests).
5. **Two sessions in one workspace** — a Codex flag lands in whichever
   session's hook ran last. Pinned by a test so the behaviour is a decision,
   not an accident (Task 4).
6. **Claude running a `Bash` call and an `Edit` at once** — the edited file
   falls inside the shell's window and is also reported as a write. Counted
   twice, ranked a little hot; documented rather than fixed (Task 6).

---

## File Structure

**Created:**

- `crates/thirdeye-adapters/src/codex.rs` — Codex payload → `Payload`.
- `crates/thirdeye/src/observe.rs` — tool-call markers and the mtime window.
- `crates/thirdeye/src/cmd_codex.rs` — `thirdeye codex install | uninstall`.
- `agents/codex/hooks.json`, `agents/codex/config.toml`,
  `agents/codex/skills/flagging-files/SKILL.md` — templates the binary embeds.
- `scripts/e2e-codex.sh` — the live check, mirroring `scripts/e2e.sh`.

**Modified:**

- `crates/thirdeye-adapters/src/lib.rs` — `Payload`, `Lifecycle` (moved out of
  `claude.rs`), the new `Observe`, and `translate`.
- `crates/thirdeye-adapters/src/claude.rs` — uses the shared types.
- `crates/thirdeye/src/cmd_hook.rs` — `--agent`, observation, active pointer.
- `crates/thirdeye/src/notify.rs` — `stop` no longer blocks.
- `crates/thirdeye/src/session_ref.rs` — the workspace's active session.
- `crates/thirdeye/src/cmd_mcp.rs` — session fallback, resolved per call.
- `crates/thirdeye/src/main.rs` — `hook --agent`, `codex` subcommand.
- `xtask/src/agent.rs` — `Codex` becomes a supported agent.
- `docs/install.md`, `docs/e2e.md`, `CLAUDE.md`.

---

## Task 1: Adapter selection

**Files:**
- Modify: `crates/thirdeye-adapters/src/lib.rs`, `crates/thirdeye-adapters/src/claude.rs`
- Modify: `crates/thirdeye/src/cmd_hook.rs`, `crates/thirdeye/src/main.rs`

**Interfaces:**
- Produces (in `thirdeye_adapters`):
  - `pub struct Payload { session, cwd, records, lifecycle, observe: Option<Observe> }` — today's fields, moved, plus `observe`.
  - `pub enum Lifecycle { Start, End }` — moved.
  - `pub enum Observe { Begin { call: String }, End { call: String } }`
  - `pub fn translate(agent: &AgentId, raw: &str, now: Timestamp) -> Result<Payload, AdapterError>`
  - `AdapterError::NoAdapter(String)` for an `AgentId` with no adapter.
- Produces (binary): `thirdeye hook [--agent claude|codex]`, default `claude`.

- [ ] **Step 1: Write the failing tests**

In `lib.rs`:

```rust
#[test]
fn an_agent_without_an_adapter_is_an_error_not_a_guess() {
    let err = translate(&AgentId::Cursor, "{}", Timestamp(1)).unwrap_err();
    assert!(matches!(err, AdapterError::NoAdapter(_)));
}

#[test]
fn claude_payloads_translate_exactly_as_before() {
    // The dispatch must be a pure routing layer; every Claude test still
    // passes through `claude::records_from_payload` unchanged.
    let raw = r#"{"session_id":"s","cwd":"/w","prompt_id":"p","hook_event_name":"PostToolUse",
                  "tool_name":"Write","tool_input":{"file_path":"/w/a"},"tool_use_id":"t"}"#;
    assert_eq!(
        translate(&AgentId::Claude, raw, Timestamp(1)).unwrap(),
        claude::records_from_payload(raw, Timestamp(1)).unwrap()
    );
}
```

In `main.rs` (add a `#[cfg(test)] mod tests`):

```rust
#[test]
fn a_bare_hook_is_a_claude_hook() {
    // Every installed Claude plugin runs `thirdeye hook` with no flag.
    let cli = Cli::try_parse_from(["thirdeye", "hook"]).unwrap();
    assert!(matches!(cli.command, Command::Hook { agent: HookAgent::Claude }));
}

#[test]
fn codex_is_named_explicitly() {
    let cli = Cli::try_parse_from(["thirdeye", "hook", "--agent", "codex"]).unwrap();
    assert!(matches!(cli.command, Command::Hook { agent: HookAgent::Codex }));
}
```

`run_inner` gains an `agent: &AgentId` parameter; existing `cmd_hook` tests
pass `&AgentId::Claude` and must otherwise stay unchanged.

- [ ] **Step 2: Run them — expect compile errors** (`translate`, `NoAdapter` missing).

Run: `cargo test -p thirdeye-adapters && cargo test -p thirdeye cmd_hook`

- [ ] **Step 3: Implement**

Move `Payload` and `Lifecycle` to `lib.rs` (re-export from `claude` is not
needed — update its imports). `translate` matches `AgentId::Claude` and
`AgentId::Codex` (the latter added in Task 2; until then it returns
`NoAdapter`). `Command::Hook` becomes `Hook { #[arg(long, value_enum, default_value
= "claude")] agent: HookAgent }` with `enum HookAgent { Claude, Codex }`
(`clap::ValueEnum`) mapped to `AgentId`. `run_inner` gains `agent: &AgentId` and calls
`translate`.

- [ ] **Step 4: Gates green, then commit**

```bash
git commit -am "refactor(adapters): choose an adapter by --agent, default claude"
```

---

## Task 2: The Codex adapter

**Files:**
- Create: `crates/thirdeye-adapters/src/codex.rs`
- Modify: `crates/thirdeye-adapters/src/lib.rs` (`pub mod codex;`, route `AgentId::Codex`)

**Interfaces:**
- Consumes: `Payload`, `Lifecycle`, `Observe` (Task 1).
- Produces: `pub fn records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError>`

Payload shapes are in `docs/codex-hook-payload.md`; build test inputs from it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_prompt_opens_a_turn_keyed_by_turn_id()
    // UserPromptSubmit → one LogRecord::Turn, TurnKind::Open { prompt }, id == turn_id,
    // session == session_id.

#[test]
fn stop_closes_the_turn_it_belongs_to()
    // Stop → TurnKind::Close with id == turn_id.

#[test]
fn a_tool_call_is_bracketed_not_interpreted()
    // PreToolUse (tool_name "Bash") → observe == Some(Observe::Begin { call: tool_use_id }),
    // no records. PostToolUse → Observe::End { call }, no records.

#[test]
fn a_command_output_never_reaches_the_log()
    // PostToolUse whose tool_response is "alpha\nbeta\n": serialize every record
    // of the Payload and assert none contains "alpha".

#[test]
fn session_end_is_a_lifecycle_not_a_record()
    // SessionEnd → lifecycle == Some(Lifecycle::End), records empty.

#[test]
fn a_tool_call_without_an_id_is_not_observed()
    // PostToolUse lacking tool_use_id → observe None. A window with no key
    // could never be closed.
```

- [ ] **Step 2: Run — expect compile errors.** `cargo test -p thirdeye-adapters codex`

- [ ] **Step 3: Implement** `records_from_payload`: `session_id`, `cwd` as in
Claude; `UserPromptSubmit` → open marker (no mention extraction — Codex has no
captured `@` syntax); `Stop` → close marker; `PreToolUse`/`PostToolUse` →
`Begin`/`End` keyed on `tool_use_id`, for every tool; `SessionStart`/
`SessionEnd` → lifecycle; anything else → empty `Payload`. Route
`AgentId::Codex` in `translate`.

- [ ] **Step 4: Gates green, commit.** `git commit -m "feat(adapters): codex — turns, lifecycle, and bracketed tool calls"`

---

## Task 3: Watching the filesystem for what a tool call wrote

**Files:**
- Create: `crates/thirdeye/src/observe.rs`
- Modify: `crates/thirdeye/src/cmd_hook.rs`, `crates/thirdeye/src/notify.rs`, `crates/thirdeye/src/main.rs` (`mod observe;`)

**Interfaces:**
- Consumes: `Observe` (Task 1).
- Produces (`observe.rs`):
  - `pub fn begin(markers: &Path, call: &str) -> std::io::Result<()>` — creates `markers/<marker_name(call)>`.
  - `pub fn end(markers: &Path, call: &str, root: &Path, now: SystemTime) -> Vec<PathBuf>` — absolute paths of regular files under `root` with `marker_mtime <= mtime <= now`; removes the marker; empty if there was none.
  - `pub fn clear(markers: &Path)` — removes the directory.
  - `pub fn markers_for(socket: &Path) -> PathBuf` — `socket.with_extension("calls")`, beside the socket and the session pointer.
  - `fn marker_name(call: &str) -> String` — every byte outside `[A-Za-z0-9._-]` becomes `_`, truncated to 96 bytes; call ids are agent-supplied.
  - `fn candidates(root: &Path, cap: usize) -> Vec<PathBuf>` — the files worth `stat`ing, as below; `end` calls it with `WALK_CAP`.
  - `const WALK_CAP: usize = 50_000`

Candidates: if `git -C root rev-parse --is-inside-work-tree` succeeds, the
output of `git -C root ls-files -z --cached --others --exclude-standard`;
otherwise a recursive walk that skips every directory whose name starts with
`.` and stops after `WALK_CAP` entries. Each candidate is `stat`ed; only
regular files inside the window are returned.

- [ ] **Step 1: Write the failing tests** (in `observe.rs`, against temp dirs)

```rust
fn a_file_written_inside_the_window_is_reported()
fn a_file_written_before_the_call_began_is_not()
    // write file, sleep 20 ms, begin, end → not reported.
    // Perturbation: drop the lower bound and this goes red.
fn an_ignored_file_is_never_reported()
    // git init; .gitignore "target/"; write target/x inside the window → absent.
fn outside_a_repository_hidden_directories_are_not_walked()
    // no git; write .venv/x and a.txt in window → only a.txt.
fn the_walk_stops_at_its_cap()
    // no git; 10 files; candidates(root, 3).len() == 3.
fn ending_a_call_that_never_began_reports_nothing()
fn a_hostile_call_id_cannot_escape_the_marker_directory()
    // begin(markers, "../../etc/x") → the only file created is inside `markers`.
fn the_marker_is_gone_after_the_call_ends()
```

In `cmd_hook.rs`:

```rust
fn a_codex_tool_call_records_the_files_it_wrote()
    // PreToolUse payload, write <ws>/notes.md, PostToolUse payload → the log holds
    // one FileEvent: kind Write, source Hook, prompt_id == turn_id,
    // dedup_key == "exec-1:<ws>/notes.md".
fn a_session_ending_clears_its_markers()
```

`notify.rs`: `stopping_does_not_wait_for_the_unit` — extract
`fn stop_args(instance: &str) -> Vec<String>` and assert it contains
`"--no-block"`. Codex runs `SessionEnd` synchronously with a 1–3 s budget; a
blocking `systemctl stop` that overran it would be killed and leave the daemon
running.

- [ ] **Step 2: Run — expect failures.** `cargo test -p thirdeye observe cmd_hook notify`

- [ ] **Step 3: Implement** `observe.rs` as specified; in `run_inner`, when
`payload.observe` is `Begin` call `begin`, when `End` call `end` with the
workspace root and `SystemTime::now()` taken **before** walking, and append one
`FileEvent` per path (fields per Global Constraints). Observation needs a
session (for the socket path); with none, skip it. The `SessionEnd` branch
calls `clear` beside the pointer removal. `stop` passes `--no-block`.

- [ ] **Step 4: Perturbation** — remove the lower bound of the window; confirm
`a_file_written_before_the_call_began_is_not` goes red; restore.

- [ ] **Step 5: Gates green, commit.** `git commit -m "feat(hook): report the files a tool call wrote, by watching the workspace"`

---

## Task 4: A flag finds the workspace's latest session

**Files:**
- Modify: `crates/thirdeye/src/session_ref.rs`, `crates/thirdeye/src/cmd_hook.rs`, `crates/thirdeye/src/cmd_mcp.rs`

**Interfaces:**
- Produces (`session_ref.rs`):
  - `pub fn record_active(workspace_dir: &Path, session: &str) -> std::io::Result<()>` — writes `<workspace_dir>/active-session` (the bare id), skipping the write when unchanged; creates the directory `0700` and the file `0600`, like every segment (the log's owner-only rule).
  - `pub fn load_active(workspace_dir: &Path) -> Option<String>`
- `cmd_mcp::Server.session` becomes resolved per call: the environment's
  `CLAUDE_CODE_SESSION_ID` if set, else `load_active(paths.workspace_dir())`.

- [ ] **Step 1: Write the failing tests**

```rust
// session_ref.rs
fn the_last_session_to_act_is_the_active_one()
    // record_active(a), record_active(b) → load_active == Some("b").
    // Pins Review Focus 5: two sessions in one workspace, latest wins.
fn a_workspace_with_no_session_has_no_active_one()

// cmd_mcp.rs
fn a_flag_with_no_session_in_its_environment_joins_the_active_one()
    // Server built with session None; record_active(ws_dir, "s9"); flag → the
    // event's session == Some("s9") and it lands in s9's directory.
fn an_explicit_session_outranks_the_active_one()
    // env session "s1", active "s9" → s1.

// cmd_hook.rs
fn every_hook_marks_its_session_active()
```

- [ ] **Step 2: Run — expect failures.**

- [ ] **Step 3: Implement.** `run_inner` calls `record_active` for any payload
with a session (best-effort, ignored errors, before the append). In
`cmd_mcp`, keep the env lookup at startup but resolve the fallback inside the
tool call, because Codex starts MCP servers before the first hook has run.

- [ ] **Step 4: Gates green, commit.** `git commit -m "feat(mcp): a flag with no session joins the workspace's active one"`

---

## Task 5: `thirdeye codex install` and the installer

**Files:**
- Create: `crates/thirdeye/src/cmd_codex.rs`, `agents/codex/hooks.json`,
  `agents/codex/config.toml`, `agents/codex/skills/flagging-files/SKILL.md`
- Modify: `crates/thirdeye/src/main.rs`, `xtask/src/agent.rs`, `docs/install.md`

**Interfaces:**
- `thirdeye codex install [--project <dir> | --user]` and `uninstall` with
  the same flags. `--project` writes under `<dir>/.codex/`; `--user` under
  `$CODEX_HOME` (default `~/.codex`).
- `pub fn install(codex_dir: &Path, binary: &Path) -> Result<()>` and
  `pub fn uninstall(codex_dir: &Path) -> Result<()>` in `cmd_codex.rs`.
- Templates are `include_str!`'d; `{{THIRDEYE}}` is replaced with this
  binary's path, as `cmd_units` rewrites `ExecStart`.

Template content:

- `hooks.json` — events `UserPromptSubmit`, `PreToolUse`, `PostToolUse`,
  `Stop`, `SessionEnd`, each one command hook:
  `{{THIRDEYE}} hook --agent codex 2>>"${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye/hook-errors.log" || true`.
  `PreToolUse` without `async`; `SessionEnd` with `"timeout": 3`; the rest
  `"async": true`. No matcher: every tool can write a file.
- `config.toml` — one block, delimited by `# >>> thirdeye` / `# <<< thirdeye`:
  `[mcp_servers.thirdeye]` with `command = "{{THIRDEYE}}"`, `args = ["mcp"]`.
- `SKILL.md` — the Claude skill's text with the tool named neutrally ("the
  thirdeye `flag` tool"), since Codex does not use `mcp__…` names.

Merging, because these files may already hold the user's own settings:
`hooks.json` — parse with `serde_json`, drop every hook whose command contains
`hook --agent codex`, add ours, write back. `config.toml` — remove any
existing delimited block, append ours; no TOML parser (no new crate), and the
delimiters are what uninstall removes. Skills — copy the directory.

- [ ] **Step 1: Write the failing tests** (`cmd_codex.rs`, temp dirs)

```rust
fn installing_twice_leaves_one_set_of_hooks()
fn the_users_own_hooks_survive_install_and_uninstall()
    // pre-existing PreToolUse hook "my-linter" is present after both.
fn the_users_own_config_survives_install_and_uninstall()
    // pre-existing `model = "x"` line is byte-identical after both.
fn the_hooks_run_this_binary()
    // every installed command starts with the given binary path.
fn the_template_still_names_the_placeholder_the_installer_rewrites()
    // as cmd_units does for ExecStart.
fn pre_tool_use_is_synchronous_and_session_end_has_its_budget()
    // parse installed hooks.json: PreToolUse has no "async"; SessionEnd timeout == 3.
```

`xtask/src/agent.rs`: replace `exactly_one_agent_is_supported_today` with
`claude_and_codex_are_supported` and drop the two tests that use `codex` as
the unsupported example (use `opencode`).

- [ ] **Step 2: Run — expect failures.**

- [ ] **Step 3: Implement** `cmd_codex.rs` and the subcommand. xtask's `Codex`
agent: `install` runs `<prefix>/bin/thirdeye codex install --project
<project>` (scope `local`) or `--user` (scope `user`); `uninstall` likewise.

- [ ] **Step 4: Document** in `docs/install.md`: Codex must trust the project
(it asks on first run), and **every install or update must be approved in
`/hooks`** — trust is keyed on a hash of the hook definition, so a changed
`hooks.json` is skipped until re-approved. Restart Codex after installing.

- [ ] **Step 5: Gates green, commit.** `git commit -m "feat(codex): thirdeye codex install, and the installer wires it"`

---

## Task 6: Shell mutations for every agent, starting with Claude Code

Claude Code edits through `Bash` too (`sed -i`, redirects, scripts), and the
Claude adapter ignores `Bash` today, so those writes never reach the log. The
observer of Task 3 is agent-neutral — any adapter that emits `Observe` gets
it — so this is one adapter arm and two hook entries. **The rule for every
future adapter:** structured tools are translated; anything that runs
arbitrary commands is bracketed.

**Files:**
- Modify: `crates/thirdeye-adapters/src/claude.rs`, `agents/claude/hooks/hooks.json`,
  `agents/claude/.claude-plugin/plugin.json` (version 0.7.0), `scripts/e2e.sh`

**Interfaces:**
- Consumes: `Observe` (Task 1), the observer wiring in `cmd_hook` (Task 3).
- Claude's `PreToolUse` for `Bash` → `Observe::Begin { call: tool_use_id }`;
  `PostToolUse` for `Bash` → `Observe::End`. No `FileEvent` for `Bash` itself.

- [ ] **Step 1: Write the failing tests** (`claude.rs`)

```rust
fn a_shell_call_is_bracketed()
    // PreToolUse Bash → Observe::Begin{ call: "toolu_1" }; PostToolUse Bash → End.
fn structured_tools_are_not_bracketed()
    // PostToolUse Edit → a FileEvent as before, observe None. Bracketing it too
    // would report every Claude edit twice.
fn a_shell_calls_output_never_reaches_the_log()
    // PostToolUse Bash with tool_response.stdout "secret" → no record contains it.
```

- [ ] **Step 2: Run — expect failures; implement.** Match `tool_name == "Bash"`
  before the structured-tool mapping.
- [ ] **Step 3: Wire the plugin.** `hooks.json`: add `"PreToolUse"` with
  matcher `^Bash$`, **not** `async` (it must land before the command runs),
  and add `Bash` to the existing `PostToolUse` matcher. Bump the plugin to
  `0.7.0` (CLAUDE.md: an unchanged version is never re-copied).
- [ ] **Step 4: Extend `scripts/e2e.sh`** — the prompt also asks Claude to
  append a line to `data.txt` *using a shell command*; assert `data.txt`
  appears in the log as `write` (fail) — the only way it can is through the
  observer.
- [ ] **Step 5: Gates green, `cargo xtask install`, commit.**
  `git commit -m "feat(claude): report the files a shell command wrote"`

---

## Task 7: Live verification

**Files:**
- Create: `scripts/e2e-codex.sh`
- Modify: `docs/e2e.md`, `CLAUDE.md`, `docs/codex-hook-payload.md` (only if the run contradicts it)

`scripts/e2e-codex.sh` mirrors `scripts/e2e.sh` and borrows
`scripts/codex-capture.sh`'s isolation: a throwaway `CODEX_HOME` with
`auth.json` linked, a `config.toml` that marks the scratch project trusted
(`[projects."<path>"] trust_level = "trusted"`), `CLAUDE*` stripped, stdin
from `/dev/null`, `--dangerously-bypass-hook-trust --skip-git-repo-check
--sandbox workspace-write`. It runs `thirdeye codex install --project
<scratch>`, then one `codex exec` asking Codex to read `data.txt`, create
`notes.md` and edit `data.txt`, and to flag `notes.md` for the human.

- [ ] **Step 1: Write the script** with these assertions (same `pass`/`soft`/`fail` vocabulary):
  - fail: no `hook.jsonl`; no turn open/close markers sharing a `turn_id`;
    `notes.md` and `data.txt` absent from the log as `write`; any line
    containing `alpha` (command output leaked); a line over 4096 B.
  - soft: no flag (the model's choice); flag present but not in the session
    directory (the active-pointer fallback failed — make this a **fail** once
    one run has shown it working).
  - pass/fail: the daemon's journal says it opened the view; `thirdeye ls`
    renders both files.
- [ ] **Step 2: Run it.** Fix what it finds; record anything that contradicts
  `docs/codex-hook-payload.md` there.
- [ ] **Step 3: Check a surface by hand** — run `codex` inside tmux in a
  project wired with `thirdeye codex install`, approve the hooks in `/hooks`,
  ask for an edit; the pane must open beside it. Record the result in
  `docs/tmux.md` beside the Claude one.
- [ ] **Step 4: Update CLAUDE.md** — the Adapter seam row (`claude.rs`,
  `codex.rs`), delete "Known gap: adapters have no dispatch", a **Built**
  paragraph for Codex and for shell observation, the bracketing rule for
  future adapters, and two gotchas: Codex hooks need re-approval in `/hooks`
  after every change, and observed writes are mtime-windowed (a human's save
  during a command is attributed to the agent).
- [ ] **Step 5: Commit.** `git commit -m "test(e2e): codex, end to end"`

---

## Not in this plan

- **Reads under Codex.** They exist only as shell commands; no read is
  recorded. Revisit with captured commands from real sessions, not guesses.
- **`apply_patch` parsing** for exact line ranges, if a capture ever shows it.
- **Doctor for Codex** — checking that `.codex/hooks.json` names the current
  binary, as it checks the units.
- **Portable activation** (macOS) and the **OpenCode plugin**, each their own
  plan.
