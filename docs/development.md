# Working on thirdeye

For someone who cloned the repo and wants to see a change running. If you just
want to *use* the tool, read `docs/install.md` instead.

## The loop

```bash
cargo xtask configure          # once, and whenever your setup changes
cargo xtask install            # every time you want your edits running
```

`configure` probes this machine, prints what it found, and writes
`install.toml`. `install` reads that file and does exactly what it says —
no probing, no defaults of its own: it builds, installs the binary with
`cargo install`, hands each named agent to **the binary's own** `thirdeye
install`, has it run `thirdeye refresh --installed-by xtask`, installs the
units, and runs `thirdeye doctor` on every configured project. Then **restart your agent**: plugins,
hooks, MCP servers and skills are read at session start and never re-read.

The split is the `./configure && make install` one, for the same reason. A
step that probes while it acts cannot be reviewed before it runs, and a machine
where a guess was wrong has nowhere to correct it. `install.toml` is that
place; it is gitignored, because it describes your machine and not the project.

The wiring itself is not xtask's. It lives in the binary
(`crates/thirdeye/src/integration/`), so a development install and a user's
`thirdeye install` take the same path; xtask only decides which agents and
projects, from `install.toml`.

### configure

```
  --prefix <dir>          where the binary goes (default: $HOME/.local)
  --profile dev|release   build profile (default: dev)
  --scope project|user    project (default), or every session the agent runs
  --agent <id>=<project>  wire an agent into a project; repeatable
  --no-units              skip systemd even if a user manager is present
```

**Nothing is wired into any agent unless you name it.** Installing into
someone's project is a side effect, so it is asked for rather than assumed:

```bash
cargo xtask configure \
    --agent claude=/path/to/one/project \
    --agent codex=/path/to/another
```

`--agent` repeats. Copilot and Grok have no project scope and are wired for
your user whatever `--scope` says; xtask says so when it does. `thirdeye
install <path>`, naming no agent, lists which agents are on this machine and
whether thirdeye supports them — detected and supported are different
facts.

Adding an agent is a file in `crates/thirdeye/src/integration/` implementing
`Integration`, a line in its `registry()`, and an adapter in
`thirdeye-adapters`; §5 of the spec makes that a seam, and neither the head
nor the daemon needs to know.

## Why the pieces agree now

Three artifacts used to install by three unrelated mechanisms — the binary,
a **copy** of the Claude plugin in Claude's cache that only a version bump
refreshed, and the units — and nothing linked them. During milestone 7 the
plugin was current, the binary was eighteen minutes stale, the hook silently
recorded no kitty address, and nothing reported a problem.

Now everything an integration loads is published by the binary into
`~/.local/share/thirdeye/`, naming the binary that published it, and Claude
Code reads its plugin live from there. So installing the binary with `cargo
xtask install` republishes everything it is wired to, every time; and a
binary that was installed some other way republishes on its first hook,
because the directory's stamp names another build. **Still never rebuild
only the binary into `target/` and expect the agents to run it**: what they
run is the installed one.

## Knowing what you are running

```bash
cargo xtask doctor          # every project install.toml names
thirdeye doctor <project>   # or one; --user for none
```

```
  ok        binary: /home/u/.local/bin/thirdeye — 0.1.0 (ea06b892-dirty)
  note      source: built from a working tree with uncommitted changes …
  ok        units: /home/u/.local/bin/thirdeye runs a binary built from ea06b892-dirty
  ok        integrations: /home/u/.local/share/thirdeye published by this build
  ok        claude: /home/u/src/app — /home/u/.local/bin/thirdeye runs a binary built from ea06b892-dirty
  unchecked session: … restart the agent after any install
```

Three things worth knowing about how it decides:

- **Integrations are compared by content** against what this build would
  install, and then by **what the agent says it loads** (`claude plugin
  list --json`, `copilot plugin list`, `grok plugin list`): files in place
  for a plugin the agent has disabled are not "fine".
- **The units are compared by what their binary reports**, not by its path.
  Running `doctor` from `target/debug` while the units point at the installed
  copy is normal; a unit launching a *stale* binary at the right path is the
  failure that actually happened.
- **`unchecked` lines are part of the output on purpose.** A report listing
  only passes would imply coverage it does not have.

## Production parity

```bash
cargo xtask configure --profile release
cargo xtask install
```

Identical steps at the release profile. Dev and production differ in profile
and prefix and nothing else — both go through one code path in
`xtask/src/main.rs`, so "a dev build matches what ships" stays true rather than
aspirational.

## Testing against an agent

Use a project you do not mind hooks firing in — not the thirdeye checkout
itself, or your development session fires hooks at itself.

```bash
cd /the/project/you/configured
claude
```

For the kitty split, `docs/kitty.md`. The head also runs standalone in any
terminal, which separates "the viewer is broken" from "the surface is broken":

```bash
thirdeye head --session <id>
```

## The end-to-end tests

```bash
scripts/test-install.sh   # install.sh, self update, self uninstall: free, no network
scripts/e2e.sh
```

`test-install.sh` serves a locally built archive over HTTP and installs it
into a scratch `HOME`, which is how the install scripts are tested without a
release.

Drives a real agent session, a real daemon and a real kitty split, then
asserts on what landed and cleans up after itself. It refuses to run if
`thirdeye doctor` reports drift, so a failure means the code is wrong rather
than something being stale. It spends Agent SDK credit — see `docs/e2e.md`.

## Gates

All four must pass; they are what "done" means here.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

`clippy::all` is `deny` and `unsafe_code` is `forbid` at the workspace level,
so lint failures are build failures. Code behind `cfg(windows)` or
`cfg(unix)` is linted only on its own platform, so it is also checked for the
other target: `rustup target add` it (`x86_64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`) and run the clippy line above with `--target <it>`;
no linker is needed. Any new indicator gets a perturbation
test — prove it goes red on a planted defect before trusting its green.

## Undoing it

```bash
cargo xtask uninstall      # units and any agent wiring, per install.toml
thirdeye self uninstall    # or: everything, thirdeye's directory included
```

`cargo xtask uninstall` leaves the binary in place deliberately; `thirdeye
self uninstall` removes your user's integrations, the units and thirdeye's
directory, and leaves an xtask-installed binary for you to delete. It cannot
unwire projects — nothing records them — so run `cargo xtask uninstall`
first.
