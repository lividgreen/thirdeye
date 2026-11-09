# Installation Plan

**Goal:** thirdeye installs the way other developer tools do. One command
gets the binary, the binary wires itself into the agents, and the binary
updates itself. Building from source keeps working.

```bash
curl -fsSL https://…/install.sh | sh                           # Linux (macOS later)
winget install thirdeye                                        # Windows
irm https://…/install.ps1 | iex                                # Windows, also
cargo install thirdeye                                         # from source

thirdeye install ~/src/app                        # detect agents, ask, wire that project
thirdeye install --user -a codex                  # or one agent, explicitly
thirdeye self update                              # new binary, then refresh every integration
```

## Decisions (2026-10-15)

1. **The repo goes public, but only when this is ready.** Until then, every
   download path must be testable without GitHub: the scripts take a local
   archive or a base URL.
2. **`thirdeye self update` is in**, because the design below keeps it small:
   it re-runs the official installer.
3. **Project scope is the default**, and `--user` is the option. Copilot and
   Grok have no project scope; `setup` says so and asks before wiring them
   per user, and never wires them silently.
4. **`cargo xtask` stays** for development, reduced to building the binary
   and handing off to the binary's own installer.
5. **No list of projects, anywhere** (2026-10-16). The receipt's
   `[[integration]]` list went stale as soon as a project moved — the e2e
   scripts' scratch projects were the first to show it — and a project list
   kept in the data directory was the wrong place for it besides. Removed
   with `uninstall --all`. `doctor <project>…` and `uninstall --project
   <dir>` are told where to look, `--user` otherwise; `self uninstall`
   unwires the user's integrations and warns that projects are the human's
   to unwire. What the receipt held besides the list moved into the stamp,
   now `published.toml`, which describes the directory it sits in.
6. **One shape for every command** (2026-10-16): the place is a path
   argument or `--user`, never assumed; agents are `-a <id>`, repeated,
   with `<id>:<dir>` for an agent run with another configuration directory.
   `setup` is gone: `install <path>` naming no agent is what it was —
   find the agents, ask about each, install the units. The tasks below
   keep the syntax they were written in.

## The rule: projects point, thirdeye's directory holds

An update must reach every project without thirdeye knowing where the
projects are. thirdeye keeps the content of every integration in **one
fixed directory of its own**, `$XDG_DATA_HOME/thirdeye/<agent>/`. A project,
or an agent's user config, holds only a **reference that does not change
between versions**: the binary's path (which `self update` replaces in
place) or the path of that directory.

An update rewrites thirdeye's directory and nothing else. Each agent then
picks up the change through its own mechanism: it reads our files in place,
or its plugin system refreshes from the directory it was pointed at. A
project that moved, was renamed or was forgotten still gets the update.

thirdeye never walks a list of projects to rewrite them. That would break
exactly when a project moves, and would leave stale wiring behind wherever
the list is wrong.

So, for every agent, the question is: **what is the smallest thing in the
project that never needs to change?** Anything that would need to change
moves into thirdeye's directory. If it cannot, it must be written so it
stays the same across versions, and a test pins that.

## Where it stands

- Six agents already install through the binary (`thirdeye <agent>
  install`), from files embedded with `include_str!`, with the binary's
  absolute path written in. So do the units.
- **Claude Code needs the checkout.** xtask runs `claude plugin marketplace
  add <checkout>`, and the plugin's scripts look for the binary on `PATH`,
  falling back to `~/.local/bin`, or `%USERPROFILE%\.local\bin` in the
  `.cmd`. The plugin carries its own version (0.8.1), unrelated to the
  crate's (0.1.0).
- **The agent seam lives in xtask** (`xtask/src/agent.rs`), so the shipped
  binary has no notion of "every agent", and the Claude install logic,
  `allow_claude_flag` included, exists only there.
- **`cargo publish` cannot work as things stand.** Fifteen `include_str!`
  paths reach `../../../agents/`, outside the crate, and a packaged crate
  contains only its own directory.
- There is no CI and no `.github/`.

## Which agents copy, which load live

This decides what "refresh after an update" means for each agent.

| Agent | Scopes | What the project holds today | Target |
|---|---|---|---|
| Claude Code | project, local, user | an enabled plugin from a marketplace **named by path**, so it is already a pointer | marketplace in thirdeye's directory. Claude **copies** the plugin into a per-user cache keyed on version, so it must refresh from that directory by itself (fact 2) |
| Copilot CLI | user | a local marketplace, loaded **live** | already right: the marketplace is in thirdeye's directory |
| Grok Build | user | a plugin Grok **copies** | the source in thirdeye's directory; Grok's own update picks it up (fact 3) |
| OpenCode, pi | project, user | a copied `index.js` with the binary's path, plus a copied skill | a fixed stub that imports the real script from thirdeye's directory (fact 7); the skill as below |
| Codex, Antigravity | project, user | hook entries running `<binary> hook --agent …`, an MCP block, a copied skill | the hook and MCP entries already name only the binary path, which stays the same; the skill as below |

**Skills are the hard part.** Codex, Antigravity, OpenCode and pi find
skills by directory, inside the project, so a skill is a copy. Ways out, to
be decided per agent after fact 8: a skill directory that is a link into
thirdeye's directory; a short skill that stays the same across versions and
tells the agent to read the full one, or to run `thirdeye`; or user-scope
skills, which live in one place per user, even for a project-scoped
install.

## Facts to capture first

Each is verified live before the task that rests on it, as every adapter
was.

1. **Claude's plugin version semantics.** Does `claude plugin update` act on
   any change of version string, or only on a semver increase? Is build
   metadata (`0.2.0+3f9a1c2e`) accepted, and does it count as a change? If
   yes, the version carries a hash of the plugin's files, and the stale-cache
   problem goes away for developers as well as for releases.
2. **Claude's marketplace auto-update.** Can a local (directory) marketplace
   be set to auto-update at session start? The rule above needs a yes. If
   the answer is no, the fallback is a single per-user `claude plugin update`
   run by `refresh`. That works only if the plugin cache is per user and not
   per project; check that too.
3. **Grok's plugin update command** and whether it takes a version from the
   manifest.
4. **Windows, winget portable installs:** does the binary's path survive
   `winget upgrade`, and is it the package directory or the `Links` shim
   that the hooks should name? (Needs the Windows machine.)
5. **Windows, replacing a running `thirdeye.exe`:** a hook-started daemon and
   any open viewer hold it. Rename-then-copy is the usual way; confirm it
   holds while a daemon runs.
6. **The daemon across a version change.** `Hello` carries
   `protocol_version`. Confirm what an old daemon does with a new head, and
   a new hook, mid-session, and that it is a clean refusal rather than a
   torn view.
7. **OpenCode and pi:** does a plugin or extension that imports another file
   by absolute path load, and does it pick up a change to that file at the
   next session?
8. **Skills by link:** do Codex, Antigravity, OpenCode and pi follow a
   symlinked skill directory? On Windows a symlink needs developer mode, and
   a junction may serve instead.

## Facts captured (2026-10-15)

1. **Claude's versions.** `claude plugin update` acts on *any* change of
   version string, a lower one included (`0.2.0+bbbb2222` → `0.1.9` updated).
   Build metadata is accepted; the cache directory spells it `0.2.0-aaaa1111`.
   Old cache directories are kept. Same version, changed files: "already at
   the latest version". Claude Code 2.1.300.
2. **Claude loads a directory marketplace live.** Better than auto-update:
   `claude plugin list --json` reports `readFromFolder: <marketplace>/<plugin>`,
   and a hook added to the folder only — absent from the cache — ran in the
   next `claude -p` with `CLAUDE_PLUGIN_ROOT` naming the folder. So an update
   is a rewrite of the folder; no `claude plugin update` is needed. Also
   found: re-adding a marketplace of the same name from another directory
   re-points it ("Plugins already installed from it now update from the new
   source"), and **`claude plugin marketplace remove --scope local` forgets
   the marketplace for every project** — another project's plugin vanished
   from its `plugin list`. Uninstall must never run it.
3. **Grok's update.** `grok plugin update [name]` on a local plugin answers
   "local symlink, already live" and changes nothing: the installed copy is
   a real directory and kept the old content, version change or not
   (1.0.50). Refreshing means uninstall and install again.
6. **The daemon across a version change.** A head or `thirdeye open` of
   another protocol gets a `Close` naming both versions — a clean refusal.
   A hook's poke was refused the same way, which would have frozen an old
   daemon's view after an update; pokes are now accepted whatever their
   version, since they carry no data.
7. **OpenCode imports by absolute path:** a stub `index.js` with `export {
   default } from "file:///…/plugin.mjs"` loads, and `scripts/e2e-opencode.sh`
   passes through it (2.0.27). A bare absolute path works as well as a
   `file://` URL, and so does `import x from …; export default x`. pi
   (1.1.0) loads all three forms too, once the project is approved
   (`--approve`), and both pick up an edit to the imported file at the next
   session. **A running OpenCode server reloads the plugin by itself** when
   the imported file changes — the shared background service included — so
   a republish reaches it without a restart. Whether the old instance's
   hooks are torn down first was not checked; events counted twice after an
   update would be that.
8. **Skills by link.** A symlinked skill *directory* is followed by all
   four: OpenCode (`skill.list`), pi (`get_commands`), Codex 0.162.1
   (`codex debug prompt-input`) and Antigravity 1.3.3 (asked the model, the
   only agent with no listing). A real directory whose `SKILL.md` is a
   symlinked *file* works for the other three but **not for Codex**, which
   leaves the skill out — so `skills.rs` links the directory, never the
   file. Windows (developer mode, junctions) is still open.

Still open: 4 and 5, and 8 on Windows (the Windows machine) — see the
status below.

## Layout on disk

| What | Linux | Windows |
|---|---|---|
| Binary | `$XDG_BIN_HOME/thirdeye`, else `~/.local/bin/thirdeye` | `%LOCALAPPDATA%\Programs\thirdeye\thirdeye.exe` (script) or winget's path |
| thirdeye's own integration files | `$XDG_DATA_HOME/thirdeye/` (`claude/`, `copilot/`, `published.toml`) | `%LOCALAPPDATA%\thirdeye-data\` |
| Logs | `$XDG_STATE_HOME/thirdeye/` (unchanged) | `%LOCALAPPDATA%\thirdeye\` (unchanged) |
| Units | `$XDG_CONFIG_HOME/systemd/user/` (unchanged) | none |

## The receipt

*Superseded by decision 5: the list below is gone, and the rest is
`published.toml` (`build`, `binary`, `source`). Kept as designed, for the
record.*

`receipt.toml` records what was installed and how:

```toml
version = "0.2.0"
binary = "/home/me/.local/bin/thirdeye"
source = "script"            # script | cargo | winget | xtask

[[integration]]
agent = "claude"
scope = "project"
project = "/home/me/src/app"
```

**Updates never read the integration list.** By the rule above, an update
rewrites thirdeye's directory, and that is all it does. The list exists for
two things: so `uninstall --all` and `self uninstall` can offer to remove
wiring they would otherwise have to search for, and so `doctor` can report
it. Both treat it as a hint. A project listed there that has moved is
reported, not an error. `source` tells `self update` which tool owns the
binary.

## Tasks

### Milestone A — the binary installs itself (all verifiable here, now)

**A0. CI.** `.github/workflows/ci.yml`: fmt, clippy and test on
`ubuntu-latest` and `windows-latest`. Both targets are already a gate by
hand; this makes them one automatically. It runs on the private repo
already.

**A1. The agent seam moves into the binary.** New directory
`crates/thirdeye/src/integration/`. `mod.rs` holds the trait and nothing
else, following the seam rule. One file per agent:

```rust
pub trait Integration {
    fn id(&self) -> &'static str;
    fn program(&self) -> &'static str;              // what detection looks for
    fn scopes(&self) -> &'static [Scope];           // Copilot, Grok: [User]
    fn install(&self, target: &Target, binary: &Path) -> Result<Installed>;
    fn uninstall(&self, target: &Target) -> Result<()>;
    fn publish(&self, binary: &Path) -> Result<()>; // write thirdeye's own directory for this agent
    fn inspect(&self, target: &Target) -> Inspection; // for doctor
}
```

`publish` writes everything that changes between versions into
`$XDG_DATA_HOME/thirdeye/<agent>/`. `install` calls it, then writes only
the pointer into the target. A test for each agent installs, publishes
again with different content, and asserts that the target's files are
byte for byte the same as before. That is the rule, enforced.

The bodies come from the existing `cmd_<agent>.rs` files, which shrink to
nothing and go. `xtask/src/agent.rs` goes with them. The CLAUDE.md rule that
agents live behind a trait still holds; it simply lives where the installer
now is. The `Unsupported` agents (detected, not served) move too.

**A2. Claude Code from embedded files.** Claude's install writes a local
marketplace to `$XDG_DATA_HOME/thirdeye/claude/`, as Copilot's install
already does, then runs `claude plugin marketplace add <dir> --scope <s>`
and `claude plugin install thirdeye@thirdeye --scope <s>`, and allows
`flag` (`allow_claude_flag`, moved from xtask).

- The hook and MCP scripts are generated with the binary's absolute path in
  them. The `PATH` lookup and the `~/.local/bin` fallback go, in both the
  `.sh` and the `.cmd`. A hook still exits 0 whatever happens.
- The manifest's `version` is written at install time: the crate version,
  plus a hash of the plugin's files if fact 1 allows it.
- The root `.claude-plugin/marketplace.json` goes. One way to install, not
  two that drift apart.

**A3. Embedded files move inside the crate.** `agents/` moves under
`crates/thirdeye/` (or the crate gets a `build.rs` that copies them), so
`cargo package --list` contains them. A test asserts that every embedded
file is inside the package, because `cargo publish` failing on release day
is the alternative.

**A4. The front door.**

- `thirdeye setup`: lists the agents on this machine, marking which are
  supported. It wires the chosen ones into the current workspace (resolved
  through `workspace.rs`, like every other command). For Copilot and Grok it
  asks "per user?" and does not assume. It installs the units where a
  systemd user manager runs. It prints what each agent still needs (restart
  it; for Codex, approve the hooks in `/hooks`). Interactive input is read
  from the terminal, not stdin, because under `curl … | sh` stdin is the
  script. Without a terminal it prints what it would do and exits.
- `thirdeye install <agent>… [--project DIR | --user]` and `thirdeye
  uninstall <agent>… | --all`: the scriptable forms. The default target is
  the current workspace.
- `thirdeye refresh`: runs `publish` for every agent, which rewrites
  thirdeye's directory and touches no project. It is idempotent. For an
  agent that copies (Claude if fact 2 says no, Grok), it then runs that
  agent's own per-user update command, once, never once per project.
- `thirdeye <agent> install|uninstall` are removed. thirdeye is before 1.0,
  and two spellings of one command is how docs go stale.
- `doctor` reads the receipt. If the receipt's version is not this binary's,
  it says to run `thirdeye refresh`.

**A5. xtask shrinks.** `cargo xtask install`: build, copy the binary to
the prefix, then run `<binary> install <agent> --project <p>` for each
`install.toml` entry, then `<binary> units install`, then `<binary>
doctor`. That covers binary, plugin and units in one command, as now. The
uninstall-then-install workaround for Claude's cache goes, if fact 1 lets
the content hash do the job.

**A6. Docs.** Quick start in `README.md`; `docs/install.md` rewritten
around `setup`, with each agent's section keeping only what is specific to
it; `docs/development.md`; CLAUDE.md (Commands, the seam table, and the
Gotchas about the plugin cache).

### Milestone B — releases, scripts, self update (testable before going public)

**B1. Release workflow.** `.github/workflows/release.yml`, on a `v*` tag:

- Linux: `x86_64` and `aarch64-unknown-linux-musl`, static, so one archive
  runs on any distribution.
- Windows: `x86_64` and `aarch64-pc-windows-msvc`.
- Output: `thirdeye-<version>-<target>.tar.gz` or `.zip`, a `.sha256` for
  each, and both installer scripts, attached to a **draft** release.

Hand-written rather than generated by cargo-dist. The installers are ours
to shape (XDG paths, the receipt, the hand-off to `refresh`), and
`self update` runs them. A generated installer would have to be wrapped or
patched for both.

**B2. `install.sh`.** POSIX `sh`, no bash.

- Picks the target from `uname`.
- Downloads the archive and its checksum, and verifies with `sha256sum` or
  `shasum -a 256`; a mismatch stops the install.
- Installs atomically: writes to a temporary file in the target directory,
  then `mv`.
- Then runs `<new binary> refresh --installed-by script`, which republishes
  thirdeye's directory. On a first install it offers
  `thirdeye setup`, reading from `/dev/tty`.
- Warns if the binary's directory is not on `PATH`, and does not edit shell
  rc files.
- Knobs: `THIRDEYE_VERSION`, `THIRDEYE_INSTALL_DIR`, `THIRDEYE_BASE_URL`, and
  `--archive FILE`. The last two are how it is tested against a local
  `python3 -m http.server` while the repo is private.
- Tested by a script that serves a locally built archive and installs into
  a scratch `HOME` and `XDG_*`.

**B3. `install.ps1`.** The same on Windows:

- Installs to `%LOCALAPPDATA%\Programs\thirdeye\`, and adds that directory
  to the **user** `PATH` through the registry, not `setx`, which truncates.
- Replaces a running `thirdeye.exe` by renaming it first (fact 5). The old
  file is removed on the next run.
- Verified on the Windows machine.

**B4. `thirdeye self update [--version V]`.** It reads `source` from the
receipt:

- `script`: runs the official installer for this platform, with
  `THIRDEYE_VERSION` set. Linux: `curl -fsSL <url> | sh`. Windows: `powershell
  -NoProfile -Command "irm <url> | iex"`. The installer replaces the binary
  and runs the *new* binary's `refresh`. On Linux the running binary's
  `current_exe` reads `… (deleted)` after the swap, which is why the refresh
  belongs to the new binary, not this one.
- `cargo`, `winget`, `xtask`: refuses, and prints the command that owns the
  update (`cargo install thirdeye --force`, `winget upgrade thirdeye`,
  `cargo xtask install`). A package manager that finds its file replaced
  behind its back is a support ticket.

No HTTP client enters the binary: `curl` is what fetched the installer in
the first place, and Windows 10 ships PowerShell.

**B5. `thirdeye self uninstall [--purge]`.** Runs `uninstall --all` from the
receipt, then `units uninstall`, then removes the data directory and the
binary (on Windows, by renaming it and leaving it for deletion). `--purge`
also removes the logs. If the source is `winget` or `cargo`, it removes the
integrations and tells the human to uninstall the binary through that tool.

**B6. Refresh without a post-install hook.** `winget upgrade` and `cargo
install` run nothing of ours afterwards, so thirdeye's directory still holds
the previous version's files. The binary records which version last
published the directory, in a stamp file inside it. The first `thirdeye
hook` of a session that finds a different version runs `publish` itself:
writing a few files inside thirdeye's own directory is cheap, and touches
no project. Copying agents then refresh through their own mechanism, at the
latest at the next session. If that proves too slow for a hook, `doctor`
reports the mismatch instead and `thirdeye refresh` fixes it.

### Milestone C — going public

**C1. History before visibility.** origin carries `archive/full-history`
(137 commits, 36 of them with a `Claude-Session:` trailer) and
`archive/grok-and-fixes`. `bd858eb` on `main` has a trailer too. Decide
what is visible before flipping the switch: delete the archive branches
from origin, and decide whether to rewrite `bd858eb`.

**C2. crates.io.** Publish `thirdeye-core`, then `thirdeye-adapters`, then
`thirdeye`. `xtask` gets `publish = false`. Add descriptions, keywords and
`readme`. `cargo publish --dry-run` runs in CI from A3 onwards, so this step
is a formality.

**C3. First real release.** Tag, check the draft's archives, publish.
`install.sh` and `install.ps1` are then verified against the real URLs on
both machines, including `self update` from the previous version.

**C4. winget.** Manifest with installer type `zip`, nested `portable`,
`Commands: thirdeye`. The first manifest is submitted by hand to
`microsoft/winget-pkgs`; later ones from the release workflow (`komac` or
`wingetcreate`). Verified on the Windows machine with fact 4 in hand.

## Out of scope here

- macOS. The scripts are written so that a `darwin` target is one more line
  in the target table, but there is no machine to verify it on.
- Package managers other than winget: Homebrew, Scoop, AUR, `.deb`. Easy to
  add once releases exist, and each one is a `source` in the receipt.
- Signing beyond checksums. GitHub's artifact attestations are a candidate
  for C3.
