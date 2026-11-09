# Installing thirdeye

Everything goes under `$HOME` — no `sudo`, no system paths — and every step
is reversible. There are three parts: the **binary**, an **integration per
agent**, and on Linux the daemon's **systemd user units**.

## 1. The binary

| How | Command | Updates with |
|---|---|---|
| Install script, Linux | `curl -fsSL https://github.com/lividgreen/thirdeye/releases/latest/download/install.sh \| sh` | `thirdeye self update` |
| Install script, Windows | `irm https://github.com/lividgreen/thirdeye/releases/latest/download/install.ps1 \| iex` | `thirdeye self update` |
| winget (pending) | `winget install thirdeye` | `winget upgrade thirdeye` |
| crates.io (pending) | `cargo install thirdeye` | `cargo install thirdeye --force` |
| A checkout | `cargo xtask install` ([`development.md`](development.md)) | `cargo xtask install` |

The install scripts fetch the latest release's prebuilt binary. The
crates.io and winget packages are being prepared; until they exist, `cargo
install --path crates/thirdeye --root ~/.local` from a checkout works too. On
native Windows only Claude Code is supported so far; the other agents' hooks
are written for `sh`, so use them from WSL.

The script puts the binary at `$XDG_BIN_HOME/thirdeye`, else
`~/.local/bin/thirdeye` — on Windows `%LOCALAPPDATA%\Programs\thirdeye\`,
which it adds to your user `PATH`. On Linux it never edits a shell rc file;
it tells you if the directory is not on your `PATH`. It verifies the
archive's SHA-256 before installing anything. `THIRDEYE_VERSION`,
`THIRDEYE_INSTALL_DIR` and `THIRDEYE_BASE_URL` change what it fetches and
where it goes; `install.sh --archive FILE` installs a file you already have.

Each release archive of a public release also carries a build provenance
attestation. It shows that the archive was built by this repository's release
workflow, from the release's tag, which a checksum served beside the archive
cannot:

```bash
gh attestation verify thirdeye-x86_64-unknown-linux-musl.tar.gz -R lividgreen/thirdeye
```

## 2. Wire your agents

For the project you want watched:

```bash
thirdeye install ~/src/app
```

It lists the agents on this machine, asks which to wire into that project,
installs the units where a systemd user manager runs, and says what each
agent still needs. Without a terminal to ask on (a script, CI) it wires
nothing and prints the command that would. Naming the agents asks nothing:

```bash
thirdeye install -a claude -a codex ~/src/app    # these two, that project
thirdeye install --user                          # every session the agent runs; asks which
thirdeye install --user -a copilot -a grok       # these two have no project scope
thirdeye install --user -a claude:~/.my-claude   # an agent run with another config directory
```

Every command takes its place the same way — a project's path, or
`--user` (`-u`) — and its agents as `-a <id>` (`--agent`), repeated. Agent
ids: `claude`, `codex`, `opencode`, `pi`, `copilot`, `antigravity`, `grok`.
An install merges into the agent's existing configuration, never overwrites
it, and `thirdeye uninstall` with the same place and agents removes exactly
what it added.

`-a <id>:<dir>` is for an agent you start with its configuration directory
moved: thirdeye sets the same variable the agent reads — `CLAUDE_CONFIG_DIR`,
`CODEX_HOME`, `OPENCODE_CONFIG_DIR`, `PI_CODING_AGENT_DIR`, `COPILOT_HOME`,
`GROK_HOME` — for the agent's own CLI and for every file it writes there.
Antigravity has no such variable, so `antigravity:<dir>` is refused. A `~`
at its start is your home.

**Then start a new session of the agent.** Every agent reads hooks, plugins,
MCP servers and skills at session start only.

### What goes where

**Projects point, thirdeye's directory holds.** Everything that changes from
one version of thirdeye to the next lives in one directory of thirdeye's own,
`$XDG_DATA_HOME/thirdeye/` (`~/.local/share/thirdeye/`; on Windows
`%LOCALAPPDATA%\thirdeye-data\`). A project, or an agent's own config, holds
only what stays the same across versions: the binary's path, or a path into
that directory. So an update rewrites that one directory, and every project
wired to thirdeye — wherever it is now — gets the new version at its next
session, with nothing in it rewritten.

What a project holds names paths on *this* machine — the binary, or that
directory — so keep it out of version control: Claude's goes in
`.claude/settings.local.json`, which Claude Code ignores for you; for
`.codex/`, `.opencode/`, `.pi/` and `.agents/`, add them to `.gitignore`, or
install with `--user`.

**thirdeye keeps no list of the projects it is wired into.** Nothing needs
one — an update reaches them all by rewriting that directory — and a list
would only go stale as projects move. So `doctor` and `uninstall` are told
which project to look at. The directory holds one file of its own,
`published.toml`: which build last wrote it, from which binary, and what
installed that binary.

## 3. The viewer daemon (Linux)

The daemon opens the viewer beside your agent and keeps it current. Without
it everything is still recorded, and `thirdeye ls` and `thirdeye head
--session <id>` still work, but nothing opens on its own.

`thirdeye install` installs its units when a systemd user manager runs and
they are missing (`--no-units` to leave them alone); by hand:

```bash
thirdeye units install
```

It writes two template units to `$XDG_CONFIG_HOME/systemd/user` (or
`~/.config`), pointing at wherever `thirdeye` is, and reloads the user
manager. Nothing needs enabling: the first tool call of a session starts
`thirdeye@<session>.socket`, which socket-activates the daemon. To watch
what it decides: `journalctl --user -f -u 'thirdeye@*.service'`.

**On Windows there is nothing to install**: the hook starts the daemon
itself ([`native-windows.md`](native-windows.md)). **Without systemd** — a
container, Alpine, WSL without systemd — run it yourself: `thirdeye daemon
--session <id>`.

### Where the viewer opens

Beside your agent, in whatever the agent runs in, innermost first: a tmux
pane, a Zellij pane, a WezTerm pane or a kitty window ([`tmux.md`](tmux.md),
[`zellij.md`](zellij.md), [`wezterm.md`](wezterm.md), [`kitty.md`](kitty.md)).
In Ghostty, Alacritty or GNOME Terminal, which cannot split, it opens a
separate window instead, and only when the agent flags a file unless you set
a policy ([`windows.md`](windows.md)). Two variables, set where you start the
agent, change that:

| Variable | Effect |
|---|---|
| `THIRDEYE_SURFACE` | `tmux`, `zellij`, `wezterm`, `kitty`, `ghostty`, `alacritty`, `gnome-terminal` or `none`. A surface that is not present means no viewer, not the detected one. |
| `THIRDEYE_SPLIT` | The viewer's share of the agent's pane, in percent: 40 by default, kept between 10 and 90. tmux and WezTerm size to it exactly, Zellij in 5% steps; kitty follows its own layout; a window is the size its terminal gives it. |

## Verifying it worked

```bash
thirdeye doctor .               # thirdeye, your user's wiring, and this project
thirdeye doctor --user          # thirdeye and your user's wiring only
```

It reports whether the binary, thirdeye's directory, each integration and
the units agree, and whether each agent actually loads what was installed —
for your user, and for each project you name (several may be).

Then, from a fresh session in the watched project, read or edit a file, and:

```bash
thirdeye ls
```

```
  turn 1 — Read app.rs, then change util.rs to return 43 instead of 42.…
    app.rs                                             read
    util.rs                                            edit,read
```

If nothing appears:

- Check `${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye/hook-errors.log`. A
  hook never writes to stderr — it would print into your agent session — so
  every failure lands there instead.
- Confirm you started a new agent session after installing.
- Run `thirdeye ls` from the project root, or pass its path: `thirdeye ls <path>`.

## Updating

```bash
thirdeye self update            # a binary the install script put there
```

It runs the install script again, which replaces the binary in place and has
the new binary run `thirdeye refresh`: thirdeye's directory is rewritten,
units included, and every project picks it up at its next session. For a
binary a package manager installed, `self update` refuses and names the
command that owns it — `winget upgrade` or `cargo install --force` run
nothing of thirdeye's afterwards, so the first hook the new binary runs
notices that thirdeye's directory was written by another version and
rewrites it. `thirdeye refresh` does the same by hand, and also has Grok
take a new copy of its plugin.

A session already running keeps the daemon it started with until it ends;
its hooks and viewer are the new binary's from then on.

## Uninstalling

```bash
thirdeye uninstall -a claude .          # one agent, one project
thirdeye uninstall .                    # every agent wired into it
thirdeye uninstall --user -a copilot    # what was wired for every session
thirdeye uninstall --user -a claude:~/.my-claude
thirdeye self uninstall                 # your user's wiring, the units, thirdeye's directory and the binary
thirdeye self uninstall --purge         # and the logs
```

`uninstall` always names its target: thirdeye does not know where else it
was wired. For the same reason **`self uninstall` cannot unwire your
projects**, nor agents wired in a configuration directory you named — run
`thirdeye uninstall <path>` in each project, and `thirdeye uninstall --user
-a <id>:<dir>` for each directory, before or
after. A project left wired keeps pointing at a binary and a directory that
are gone, and its agent reports a missing plugin, hook or MCP server at
every session until it is unwired.

A binary installed by cargo or winget is left for that tool to remove; `self
uninstall` says which command does it.

## Flags without a prompt

Most agents ask before running an MCP tool for the first time. A flag that
waits for approval arrives late, so each install pre-approves thirdeye's
`flag` tool, and only that tool, wherever the agent has a setting for it,
and each uninstall takes the rule out again:

| Agent | Asks by default | What the install does |
|---|---|---|
| Claude Code | yes | adds `mcp__plugin_thirdeye_thirdeye__flag` to `permissions.allow` in the settings file of the scope it installed at |
| Codex CLI | yes | sets `approval_mode = "approve"` for `flag` in its own `config.toml` block |
| Grok Build | yes, once per project | adds `MCPTool(thirdeye__flag)` to `[permission] allow` in Grok's `config.toml` |
| Antigravity CLI | yes | adds `mcp(thirdeye/flag)` to `permissions.allow` in `~/.gemini/antigravity-cli/settings.json` |
| GitHub Copilot CLI | yes | nothing: Copilot keeps approvals per repository, in `~/.copilot/permissions-config.json`, and offers no setting to pre-approve a tool everywhere. Choose "don't ask again" the first time in each repository, or start it with `--allow-tool='thirdeye(flag)'`. |
| OpenCode, pi | no | nothing is needed |

## Claude Code

`thirdeye install -a claude` publishes a local plugin marketplace into
`~/.local/share/thirdeye/claude/`, then runs `claude plugin marketplace add`
and `claude plugin install thirdeye@thirdeye` for the target. A project
install uses Claude's **local** scope, `.claude/settings.local.json`, which
Claude Code keeps out of version control: the marketplace is declared by an
absolute path into your home, which would mean nothing on a teammate's
machine. `--user` uses `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR`,
or the directory in `-a claude:<dir>`). The local scope is recorded in
Claude's configuration directory too, so for a Claude run with a moved one,
name it for a project install as well: `-a claude:<dir> <path>`, and for
its uninstall.

Claude Code loads a plugin from a local marketplace **live from its
directory** at every session start (verified against 2.1.300; `claude plugin
list --json` shows it as `readFromFolder`). So an update reaches every
project without `claude plugin update`. The manifest's version still carries
a hash of the plugin's files, so a Claude Code that runs its cached copy
instead sees a new version whenever the files change; `doctor` says when
that copy is stale.

Adding the marketplace registers it in `~/.claude/plugins/known_marketplaces.json`,
which is global. Only the plugin's enablement is per project. Uninstalling
from one project never runs `claude plugin marketplace remove`: that forgets
the marketplace for every project, and with it every other project's install.
`thirdeye self uninstall` removes it once nothing is wired.

## Codex CLI

Codex has no scriptable install for local plugins, so thirdeye merges into
Codex's own config — `<project>/.codex/` or `$CODEX_HOME` (`~/.codex`):
`hooks.json` entries that run `thirdeye hook --agent codex`, an
`[mcp_servers.thirdeye]` block in `config.toml` between `# >>> thirdeye`
markers, and the `flagging-files` skill, a link into thirdeye's directory.

The hooks name the binary by its full path, or, in a project whose `PATH`
already finds this very binary, as a bare `thirdeye` — a project's `.codex/`
is usually committed. Then, in Codex:

1. **Trust the project** — a project's `.codex/` is ignored until it is.
2. **Approve the hooks in `/hooks`.** Codex skips new or changed hooks until
   approved, keyed on a hash of each hook's definition. An update of
   thirdeye does not change them, so this is once per install.
3. **Restart Codex.**

Codex edits through shell commands, so thirdeye records the files a command
**wrote** by watching the workspace across it, and records no reads
([`codex-hook-payload.md`](codex-hook-payload.md)).

## OpenCode

OpenCode 2 has no shell hooks; it loads JS plugins into its server, and
discovers plugins and skills by directory — `<project>/.opencode/` or
`~/.config/opencode/`. thirdeye writes `plugins/thirdeye/index.js`, a stub
that imports the real plugin from thirdeye's directory, and the
`flagging-files` skill, a link; nothing is merged. The plugin forwards
OpenCode's events to `thirdeye hook --agent opencode` and defines a
`thirdeye_flag` tool, naming the binary by its full path because it runs
with the OpenCode server's `PATH`, not your shell's.

Plugins load when the OpenCode **server** starts, so restart OpenCode — and
its background service, `opencode serve --service`, if it is running.

**Known gap — the viewer and the background service.** The plugin runs in
OpenCode's server and sees the server's environment, not your terminal's.
By default that server is a shared background service: started by systemd
it has no `TMUX` or `KITTY_LISTEN_ON`, and the viewer never opens on its
own; started from a terminal, it keeps *that* terminal's. Everything is
recorded either way; open the viewer with `thirdeye head --session <id>`,
or run `opencode --standalone`, whose private server inherits your
terminal. The service also never shuts down, so a session there ends 30
minutes after its last turn.

## pi

pi loads extensions into its own process and discovers them by directory —
`<project>/.pi/` or `~/.pi/agent/`. As for OpenCode, thirdeye writes
`extensions/thirdeye/index.js`, a stub importing the real extension from
thirdeye's directory, and the skill, a link. A project's `.pi/` loads only
once the project is **trusted**: pi asks, or pass `--approve`. The
extension runs in pi's own process, so the viewer opens beside it exactly
as for Claude Code.

## GitHub Copilot CLI

Per user only — Copilot has no project-level plugins, though a repository
can switch one off:

```bash
thirdeye install --user -a copilot
```

It publishes a local marketplace into `~/.local/share/thirdeye/copilot/`
(hooks, an MCP server, the skill) and registers it with `copilot plugin
marketplace add` and `copilot plugin install thirdeye@thirdeye`. Copilot
loads a local marketplace's plugin live from that directory, so an update
needs nothing more. Start a new `copilot` session afterwards.

In a project wired for Claude Code, an interactive Copilot also loads
thirdeye's **Claude** plugin, from the project's `.claude/settings.local.json`.
That one records nothing under Copilot and offers it no `flag` tool; with
thirdeye's Copilot plugin installed, Copilot uses that one instead.

## Grok Build

Per user only:

```bash
thirdeye install --user -a grok
```

It publishes the plugin into `~/.local/share/thirdeye/grok/thirdeye/` and
runs `grok plugin install <that directory> --trust`. **Grok copies the
plugin**, and `grok plugin update` does not refresh a local one (it reports
"already live" and leaves the copy as it was; verified against 1.0.50), so
`thirdeye refresh` — which `self update` runs — removes the copy and has
Grok take a new one, once for your user. After a `cargo install` or
`winget upgrade`, run `thirdeye refresh` yourself. A Grok you run with
another `GROK_HOME` is not among them — thirdeye records no such directory
— so after an update run `thirdeye install --user -a grok:<dir>` for it
again.

The install also adds `"MCPTool(thirdeye__flag)"` to `allow` under
`[permission]` in `~/.grok/config.toml` (or `$GROK_HOME`). Start a new
`grok` session afterwards.

## Antigravity CLI

thirdeye merges into Antigravity's own files — `<project>/.agents/`, or
`~/.gemini/config/` and `~/.gemini/antigravity-cli/skills/` for `--user`: a
`thirdeye` key in `hooks.json`, a `thirdeye` server in `mcp_config.json`,
and the skill, a link. MCP tools ask before every call unless allowed, and
the rule can only live in `~/.gemini/antigravity-cli/settings.json`, so both
scopes add `mcp(thirdeye/flag)` there; only `uninstall --user` removes it.

Start a new `agy` session afterwards. Antigravity tells hooks neither the
prompt nor when a session ends, so turns show no prompt text, and the viewer
closes a few seconds after `agy` exits: the daemon watches the process.

## Skills on Windows

On Linux an agent's `skills/flagging-files` is a symbolic link into
thirdeye's directory, so the skill updates with it. Windows makes a symbolic
link a privilege, so there it is a copy, and `doctor` reports one an update
left behind; `thirdeye install <agent>` again replaces it.
