# thirdeye

[![CI](https://github.com/lividgreen/thirdeye/actions/workflows/ci.yml/badge.svg)](https://github.com/lividgreen/thirdeye/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust 1.94+](https://img.shields.io/badge/rust-1.94%2B-orange.svg)](https://www.rust-lang.org)
[![Platforms: Linux, Windows](https://img.shields.io/badge/platform-Linux%20%7C%20Windows-lightgrey.svg)](#platforms)

thirdeye shows which files a coding agent is working on, while it works.

> [!WARNING]
> thirdeye is in early development. It is tested against the agents and
> terminals listed below, but its command line, its log format and its viewer
> may change before v0.1.0. See [Status](#status).

thirdeye runs beside a command-line coding agent such as Claude Code or Codex
CLI. It keeps a live list of the files the agent has read, edited, written,
deleted or searched, grouped by prompt and ranked by how much the agent did
with each. Selecting a file opens it at the lines the agent touched, or shows
its diff. The agent can also *flag* a file it wants you to look at: a flagged
file goes to the top of the list, with the agent's reason.

The list opens by itself: as a split beside the agent in tmux, Zellij,
WezTerm or kitty, and as a separate window in terminals that cannot split.

```
┌ thirdeye — app ─────────────────────────────────────────────────────┐
│*▼ turn 2 fix the off-by-one in the parser                           │
│  ★ src/parser.rs   flag,edit,read   the bug was here, not the lexer │
│    src/lexer.rs    read                                             │
│    tests/parse.rs  write                                            │
│ ▶ turn 1 where is the input parsed?                                 │
└─────────────────────────────────────────────────────────────────────┘
```

The project is also written *3rdeye*. The binary, the crates and all
identifiers are `thirdeye`.

## Contents

- [Getting started](#getting-started)
- [Using thirdeye](#using-thirdeye)
- [What is supported](#what-is-supported)
- [Status](#status)
- [How it works](#how-it-works)
- [Privacy](#privacy)
- [Roadmap](#roadmap)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)

## Getting started

Setting up thirdeye takes three steps: install the binary, wire thirdeye into
a project, and start a new session of the agent. An agent reads its hooks,
plugins and MCP servers only when a session starts, so a session that was
already running sees nothing.

Everything is installed under your home directory. Nothing needs `sudo`.

`thirdeye install <path>` lists the agents it finds on this machine and asks
which of them to wire into that project. To skip the questions, name the
agents: `thirdeye install -a claude -a codex <path>`. Without a terminal to ask
on, it wires nothing and prints the command that would.

### Linux with systemd

```bash
curl -fsSL https://github.com/lividgreen/thirdeye/releases/latest/download/install.sh | sh
thirdeye install ~/src/app
thirdeye doctor ~/src/app
```

1. The script checks the archive's SHA-256 and installs the binary to
   `~/.local/bin/thirdeye`. If that directory is not on your `PATH`, it says
   so; it never edits a shell startup file.
2. `thirdeye install` also installs the daemon's systemd user units. Nothing
   needs enabling: the first tool call of a session starts the daemon.
3. Start a new agent session, have the agent read or edit a file, and run
   `thirdeye ls`.

### Linux without systemd

This covers containers, Alpine, and WSL without systemd. The steps are the
same, with one more.

1. Install the binary and run `thirdeye install ~/src/app` as above. No units
   are installed.
2. Start a new agent session.
3. Start the daemon yourself, once per session:
   `thirdeye daemon --session <id>`.

Without the daemon, everything is still recorded, and `thirdeye ls` and
`thirdeye head --session <id>` still work, but the viewer does not open by
itself.

### Windows

On Windows itself, thirdeye supports Claude Code, in Windows Terminal, Zellij
or WezTerm. The hooks of the other agents are written for `sh`; use those
agents from WSL.

```powershell
irm https://github.com/lividgreen/thirdeye/releases/latest/download/install.ps1 | iex
```

1. The script installs the binary to `%LOCALAPPDATA%\Programs\thirdeye\` and
   adds that directory to your user `PATH`.
2. Open a new Windows Terminal tab from Windows Terminal itself. A tab that
   `wt.exe` starts from an older shell inherits that shell's environment,
   without the new `PATH`.
3. Run `thirdeye install C:\src\app`, then start a new Claude Code session.

There is no daemon to install: the hook starts it, and the end of the session
stops it. A winget package is pending; until it is accepted, use the script.

### WSL

Inside a WSL distribution, thirdeye is a Linux program. Follow
[Linux with systemd](#linux-with-systemd), or
[Linux without systemd](#linux-without-systemd) if systemd is not enabled in
the distribution. Every supported agent works.

Windows Terminal opens the viewer as a separate window. For a split beside
the agent, run the agent inside tmux or Zellij. A Claude Code that also runs
natively on Windows needs its own installation.

### Managing an installation

```bash
thirdeye install -a claude -a codex .           # these two agents, this project
thirdeye install --user -a copilot -a grok      # every session (these two have no project scope)
thirdeye install --user -a claude:~/.my-claude  # a Claude Code run with CLAUDE_CONFIG_DIR=~/.my-claude
thirdeye doctor .                               # check that the installed pieces agree
thirdeye uninstall .                            # remove exactly what install added here
thirdeye self update                            # update the binary and every integration
```

A project holds only pointers that stay the same from one version to the
next. The files they point at live in thirdeye's own directory,
`~/.local/share/thirdeye/` (on Windows, `%LOCALAPPDATA%\thirdeye-data\`), and
an update rewrites that directory. Every wired project therefore gets the new
version at its next session, without being touched.

thirdeye keeps no list of the projects it is wired into, so
`thirdeye self uninstall` cannot unwire them. Run `thirdeye uninstall <path>`
in each project first. [`docs/install.md`](docs/install.md) describes what
each agent needs, updating, and removal in detail.

## Using thirdeye

Work with the agent as usual. When the agent edits a file, the viewer opens
beside it. In a terminal that cannot split, the viewer opens only when the
agent flags a file, because a separate window would take focus from the agent;
`thirdeye policy any` makes it open on every edit.

### In the viewer

| Key | Action |
|---|---|
| `j` `k`, arrows | move |
| `Enter`, `Space` | open a file at the lines the agent touched; fold or unfold a turn |
| `d` | switch a file between its diff and its content |
| `Esc` | return to the list |
| `q` | hide until the next turn's edits |
| `m` | hide until the next edit |
| `Q` | hide for the rest of the session |

A new flag brings the viewer back, whichever way it was hidden.

### From a shell or the agent

```bash
thirdeye ls                      # the session's files by turn, as plain text
thirdeye head --session <id>     # open the viewer yourself, in any terminal
thirdeye open                    # show the viewer now
thirdeye close [--until next-mutation|next-turn|session]
thirdeye policy any|attention|not-this-turn|never   # when the viewer may open itself
```

In Claude Code, the slash commands `/thirdeye-open`, `/thirdeye-close` and
`/thirdeye-policy` do the same. Without `--session`, each command acts on the
session that was active last in the current project; inside Claude Code, on
Claude Code's own session. `thirdeye ls` lists the ids of the others.

### Environment variables

Set these where you start the agent.

| Variable | Effect |
|---|---|
| `THIRDEYE_SURFACE` | where the viewer opens: `tmux`, `zellij`, `wezterm`, `kitty`, `ghostty`, `alacritty`, `gnome-terminal` or `none` |
| `THIRDEYE_SPLIT` | the viewer's share of the agent's pane, in percent (default 40, from 10 to 90) |
| `THIRDEYE_STATE_DIR` | where the log is kept (default `$XDG_STATE_HOME/thirdeye`) |

## What is supported

Each agent and terminal below has been verified end to end against a live
session, at the version listed.

### Agents

| Agent | Verified | Installed as | Notes |
|---|---|---|---|
| [Claude Code](https://docs.anthropic.com/en/docs/claude-code) | 2.1.295 | a plugin: hooks, an MCP server, a skill, slash commands | the reference integration |
| [Codex CLI](https://github.com/openai/codex) | 0.161.0 | `hooks.json`, an MCP server in `config.toml`, a skill | approve the hooks in `/hooks` after every install |
| [OpenCode](https://opencode.ai) | 2.0.24 | a plugin | the viewer opens by itself only under `opencode --standalone` |
| [pi](https://github.com/earendil-works/pi) | 1.1.0 | an extension | |
| [GitHub Copilot CLI](https://github.com/github/copilot-cli) | 1.0.94 | a plugin, from a local marketplace | per user only: Copilot has no per-project plugins |
| [Antigravity CLI](https://antigravity.google) (`agy`) | 1.3.2 | `hooks.json`, `mcp_config.json`, a skill | hooks receive no prompt text, so turns show none |
| [Grok Build](https://docs.x.ai/build) (`grok`) | 1.0.50 | a plugin, installed by Grok | per user only; Grok asks before the first flag in a project |

thirdeye records the same things from every agent: reads, edits (with line
ranges where the agent reports them), writes, searches, files written or
deleted by shell commands, and turns. Every agent also gets a `flag` tool,
with a skill that tells the agent when to use it.

### Terminals

The viewer opens in the innermost terminal or multiplexer the agent runs in.

| Terminal | Verified | The viewer opens as |
|---|---|---|
| [tmux](https://github.com/tmux/tmux) | 3.4 | a pane beside the agent's, unfocused |
| [Zellij](https://zellij.dev) | 0.45.1, also on Windows | a pane beside the agent's, unfocused |
| [WezTerm](https://wezfurlong.org/wezterm/) | 20260929; 20240203 on Windows | a pane beside the agent's; focus is handed back |
| [kitty](https://sw.kovidgoyal.net/kitty/) | 0.48.0 | a window in the agent's tab, unfocused (needs `allow_remote_control`) |
| [Ghostty](https://ghostty.org) | 1.3.1 | a separate window |
| [Alacritty](https://alacritty.org) | 0.13.2 | a separate window |
| GNOME Terminal | 3.52 | a separate window |
| [Windows Terminal](https://aka.ms/terminal), from WSL or Windows | 1.24 | a separate window |

Where the viewer cannot be a split, run the agent inside tmux or Zellij in
the same terminal. Both give a pane beside the agent that never takes focus,
and both work over SSH. In a terminal not listed here, they are the way to get
a viewer at all; `thirdeye head` opens one by hand.

The terminals above fall short of a split for these reasons:

| Terminal | Why the viewer is not a split beside the agent |
|---|---|
| Ghostty | no command splits a given pane; a scripting API is planned for 1.4 |
| Alacritty | it has no splits |
| GNOME Terminal | no command splits a given pane |
| Windows Terminal | `wt.exe split-pane` splits whichever pane has focus, in whatever tab or window that is, and focuses the new pane |
| kitty | it splits, but the tab's layout decides where: below the agent in the default `fat` layout, beside it in `tall` or `splits`; the size cannot be set |

### Platforms

| Platform | Status |
|---|---|
| Linux with a systemd user session | supported; verified on Ubuntu 24.04 and 26.04 |
| WSL 2 with systemd | supported; verified on Ubuntu 26.04 under WSL 3.0, in Windows Terminal, tmux and Zellij |
| Linux without systemd (containers, Alpine, WSL without systemd) | supported; the daemon is started by hand |
| Windows 10 | Claude Code only, in Windows Terminal, Zellij or WezTerm; see [`docs/native-windows.md`](docs/native-windows.md) |
| macOS | not yet supported |

## Status

thirdeye is in early development and testing. Every agent and terminal in
[What is supported](#what-is-supported) has been verified against a live
session, but thirdeye has few users so far, and each new setup is likely to
find something.

Until v0.1.0, expect the following:

- **The viewer** works, but its presentation is basic. Its layout, colours
  and key bindings are still being improved and will change.
- **The command line, the log format and the viewer protocol** may change
  between 0.0.x releases.
- **On native Windows**, only Claude Code is supported, and only Windows 10
  on x86-64 has been verified. Other agents work through WSL.
- **macOS** is not supported yet.

Prebuilt binaries for Linux and Windows are on the
[releases page](https://github.com/lividgreen/thirdeye/releases). Packages
for crates.io and winget are being prepared. Changes are listed in
[`CHANGELOG.md`](CHANGELOG.md).

## How it works

```
agent hooks ──► thirdeye hook ──► append-only log on disk ──► daemon ──► viewer
                (one adapter per agent)  (one per session)     (one per session)
```

- An agent's hooks, or its plugin, run `thirdeye hook` on every tool call.
  An *adapter* for that agent translates the payload into file events, which
  the hook appends to a log for the session. A shell command is not parsed:
  the hook records the time before the command runs, and afterwards reports
  every file in the workspace that changed in between.
- The log is the only shared state. A *daemon* per session, started on demand
  (by systemd socket activation on Linux, by the hook on Windows), derives
  everything else from it: ranking, turns, and what to show. Stopping or
  restarting the daemon loses nothing.
- The daemon opens the *viewer* through a *surface*: a tmux pane, a kitty
  window, a Ghostty window. The viewer receives newline-delimited JSON over a
  local socket, so another viewer, such as a GUI, can be written in any
  language.
- When the agent's session ends, or its process exits, the daemon closes the
  viewer and stops.

Adapters, surfaces, viewers, transports and activation are separate
interfaces, so supporting a new agent or terminal means adding a file. The
design and its rationale are in [`docs/design/`](docs/design/), and
[`CLAUDE.md`](CLAUDE.md) is the working guide to the code.

## Privacy

Everything stays on your machine; thirdeye sends nothing over the network.

The log holds every path the agent touched and the full text of your prompts,
but never file contents. It is kept under `$XDG_STATE_HOME/thirdeye`, readable
only by you (`0700` directories, `0600` files). On Windows it is kept under
`%LOCALAPPDATA%\thirdeye`, inside your user profile.

## Roadmap

**Agents**

- [ ] Cline, once its hooks settle. By default it runs tools in a shared
  background process that cannot see your terminal.
- [ ] Goose.

**Terminals**

- [ ] Konsole and Yakuake: a split through D-Bus (Konsole 24.02 or newer), a
  tab on older versions.
- [ ] xfce4-terminal: a window.
- [ ] Ptyxis: a window, once it is packaged for the distributions it can be
  tested on.
- [ ] Ghostty: a split instead of a window, when its scripting API ships.
- Not planned: Guake, which can only split the focused pane and type into it,
  and could put the viewer's command into the agent's prompt; and GNU screen,
  which tmux and Zellij cover.

**Platforms**

- [ ] A daemon started by the hook on Linux without systemd, as on Windows.
- [ ] Windows: Alacritty, and the agents other than Claude Code.
- [ ] macOS: iTerm2 (a split), Terminal.app (a window), Ghostty.
- [ ] Warp: a window at most; it has no interface that opens a pane with a
  command.

**Elsewhere**

- [ ] A transcript reader as a second source beside hooks, to recover what a
  crashed hook missed.
- [ ] An option to list the agent's reads of its own skills and
  configuration, which are hidden today.

## Documentation

| Document | Contents |
|---|---|
| [`docs/install.md`](docs/install.md) | installing, updating and removing, agent by agent |
| [`docs/native-windows.md`](docs/native-windows.md) | Windows without WSL: how the daemon starts and stops, and what was verified |
| [`docs/tmux.md`](docs/tmux.md), [`zellij.md`](docs/zellij.md), [`wezterm.md`](docs/wezterm.md), [`kitty.md`](docs/kitty.md), [`windows.md`](docs/windows.md), [`windows-terminal.md`](docs/windows-terminal.md) | each terminal: how thirdeye drives it, and what was verified |
| `docs/*-payload.md`, `docs/*-events.md` | each agent's hook payloads, as captured from a live session |
| [`docs/development.md`](docs/development.md) | the `cargo xtask` workflow for working on thirdeye |
| [`docs/e2e.md`](docs/e2e.md) | the end-to-end tests against real agents |
| [`docs/design/`](docs/design/) | the design specification, and the plan behind each part |

## Contributing

Bug reports, fixes, and support for new agents and terminals are welcome.
[`CONTRIBUTING.md`](CONTRIBUTING.md) explains how to build thirdeye, run its
checks, and add an agent or a terminal. A bug report is most useful with the
output of `thirdeye doctor`.

Report security problems privately, as [`SECURITY.md`](SECURITY.md)
describes. Everyone taking part is expected to follow the
[code of conduct](CODE_OF_CONDUCT.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
