# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

**thirdeye** watches a CLI coding agent and shows the human which files are
currently in play — ranked by how hot they are, badged with what the agent did
to them, and opened at the line range the agent actually touched.

The project is also written `3rdeye`; every crate, identifier and binary is
`thirdeye` (a leading digit is not a legal Rust identifier). Nothing here may
depend on where the checkout lives — it moves.

## Status

Built — the **capture pipeline** is end to end:

- `thirdeye-core` — event model (`event.rs`), session-keyed log (`log.rs`),
  on-disk layout (`location.rs`), ranking (`tracker.rs`), turn grouping
  (`turns.rs`).
- `thirdeye-adapters` — `claude.rs`, `codex.rs`, `opencode.rs`, `pi.rs`, `copilot.rs`, `antigravity.rs` and `grok.rs` translate payloads
  into log records; `translate` picks one by the `--agent` the hook was run
  with (default `claude`, which every installed Claude plugin relies on).
  Adapters are the *only* place an agent-specific format is read.
- `thirdeye` — the binary. `thirdeye hook` consumes a payload on stdin and
  appends; `thirdeye ls` prints the log grouped by conversational turn. `ls`
  is the headless stand-in for the TUI and the way the pipeline is verified
  before any viewer exists.
- `crates/thirdeye/agents/claude/` — the Claude Code plugin's templates:
  hooks shell out to `thirdeye hook`, `.mcp.json` starts `thirdeye mcp`. One
  directory per agent, because a second one is a sibling and `plugin/` would
  not say which. Inside the crate, so `cargo package` carries them. See
  `docs/install.md`.
- `crates/thirdeye/units/` — systemd units. thirdeye's own deployment, not an
  agent's, so they sit beside the agents rather than inside a plugin.

Built — **attention** (spec milestone 3), verified against a live session:

- `thirdeye mcp` (`cmd_mcp.rs`) — an MCP server over stdio serving one tool,
  `flag`. A second *producer*, writing `mcp.jsonl` beside `hook.jsonl`; nothing
  downstream distinguishes them. It pokes the daemon after appending, as the
  hook does: a flag is often the last thing in a turn, with no tool call
  behind it to wake anything.
- `crates/thirdeye/agents/claude/thirdeye/skills/flagging-files/` — the skill teaching Claude when to call it.
  Without it the tool exists and is never used, which is the actual risk here.
- `ls` renders flags first within their turn, starred, with the reason.

It was milestone 8, behind the daemon and TUI, and moved up because the tool
depends on nothing above milestone 2. §16 of the spec records the reasoning.

Built — the **daemon** (spec milestone 4), verified live under systemd 255:

- `daemon/` — the `Activation` and `Surface` seams, the `Hello` handshake, and
  the visibility rule of §10.2 (a mutation or a flag opens the view; reads
  alone never do). `SelfBind` binds its own socket, `SocketActivated` inherits
  systemd's through `listenfd` — which is why the workspace still forbids
  `unsafe` with no exceptions in code we write.
- `notify.rs` — the hook's second job: poke the socket after appending.
  Advisory and silent, always.
- `session_ref.rs` — the hook records the real session id and workspace beside
  the socket, because a socket-activated daemon gets only `%i` and neither the
  hashed stem nor the workspace slug inverts. Without it the daemon reads the
  wrong workspace and reports nothing while everything else works.

`SessionEnd` stops the unit, so a finished session takes its daemon and its
split with it — **but not through the daemon's own teardown**, which SIGTERM
never lets run. The head exits when its connection ends, which closes the
window it lives in, and the hook removes the session pointer. Plan and known
gaps:
`docs/design/plans/2026-08-01-daemon-skeleton.md`.

Built — the **head protocol** (spec milestone 5), verified over a real socket:

- `daemon/transport/` — the transport seam, so the whole protocol is tested
  in memory with no socket and no threads.
- `daemon/protocol.rs` — the message set and the semantic view model. Nothing
  presentational crosses the wire; the `★` is `ls`'s choice, not the model's.
- A head gets a `Snapshot` on connect, a pushed `Patch` whenever the log
  changes, and can `Select`, `ExpandTurn` and `RequestDetail`. Selection and
  folding persist to `viewstate.json`.

**Every message to a head goes through one channel and one writer thread.**
Two threads writing a newline-delimited protocol produce a torn line, not a
slow one. Heads are attached with an id and explicitly detached, because the
daemon holds a clone of each sender and the channel cannot close while it
lives.

`Detail` carries the file's last touched range as `highlight`, and the head
opens there, marking it; `DetailMode::Diff` is served from `git diff`, and
falls back to content — labelled `Content` — when there is none.

Built — the **policy engine** (spec milestone 8), verified live:

- Four modes (`any`, `attention`, `not-this-turn`, `never`), persisted to
  `policy.json`. **Events are collected in every mode** — policy governs
  visibility only, and that separation is structural: the append happens in
  the hook process before any policy code runs.
- `thirdeye open` / `close` / `policy <mode>`, which the three slash commands
  in `crates/thirdeye/agents/claude/thirdeye/commands/` shell out to. `open` and `close` bypass policy: a
  human asking outranks a rule the same human set earlier.
- A hide carries a condition the human chooses: `next-mutation`,
  `next-turn` (the default) or `session` — keys `m`, `q`, `Q` in the head,
  `thirdeye close --until` from the agent. Every condition is about
  mutations, so a new prompt alone never reopens; a **new** flag lifts any of
  them. Lifting compares counts taken from the log at hide time, so a
  restarted daemon agrees with one that never stopped, and the hide persists
  to `hide.json`. Kept apart from a `not-this-turn` suppression — conflating
  them once made a plain hide lift itself on the very next refresh.
- **A window closed by hand is a hide.** The head a surface launches says so
  in its `Hello` (`launched_by_surface`), and its leaving counts as
  `next-turn`. Only that head: `thirdeye open` hangs up straight after
  opening a window, and treating every departing head this way killed the
  pane it had just asked for.

Built — **packaging** (spec milestone 9): `thirdeye units install | uninstall
| status`. The units are embedded with `include_str!` and their `ExecStart` is
rewritten to this binary's own path, so `cargo install --root` anywhere works
unedited. A test asserts the template still contains the path the installer
rewrites — otherwise editing the unit file would silently install one pointing
at the wrong binary. Installing from a `target/` build warns, because the next
`cargo clean` would break it.

Built — the **ratatui head** (spec milestone 6): `thirdeye head --session <id>`.

- `head/state.rs` — a state machine from `(ServerMessage | Key)` to `(state,
  messages to send)`, with no ratatui and no terminal in it, so all of it is
  unit-tested.
- `head/render.rs` — every decorative decision: the `★`, fold arrows, colours,
  the split, and the scroll offset, which is recomputed per frame because it
  depends on the window. Tested against ratatui's `TestBackend`, so layout is
  asserted on a real rendered buffer.
- `head/mod.rs` — the only part that cannot be unit-tested: raw mode, the
  alternate screen, the socket. Kept correspondingly small.

The head runs in **any** terminal.

Built — the **kitty surface** (spec milestone 7), verified live against kitty
0.48.0 (`docs/kitty.md`):

- `daemon/surface/kitty.rs` — `kitten @ launch` for the split, `close-window
  --match env:THIRDEYE_SESSION=<id>` to tear it down. Matching on our own env
  marker, never the title, which a human may rename.
- The spawning is injected, so the tests assert the exact argv; the `kitten @`
  boundary itself was checked by driving a real kitty instance.
- **The split inherits kitty's environment, not the daemon's.** A daemon run
  with a non-default `$THIRDEYE_STATE_DIR` or `$XDG_RUNTIME_DIR` launches a
  head that looks elsewhere and shows nothing, next to a daemon that is
  working. Normal installs agree because both use defaults.
- `KITTY_LISTEN_ON` reaches the daemon through the session pointer, because a
  socket-activated daemon has none of the agent's environment. Its absence is
  the "no surface" signal (§12), not an error — the daemon then runs with
  `NullSurface` and still tails, groups and serves heads.

Built — the **surface registry and tmux surface**: `surface::ORDER` plus
`resolve`, and `daemon/surface/tmux.rs`, verified live against tmux 3.4
(`docs/tmux.md`). tmux reaches every terminal emulator rather than one, and
is the only surface that survives SSH — a local emulator's control socket is
unreachable from the host the agent runs on. `split-window -d` creates the
pane without taking focus, `-P -F '#{pane_id}'` yields the only precise
handle for closing it, and the session id is shell-quoted because tmux hands
the command to `sh` and session ids are agent-supplied.

Built — **Codex CLI** and **shell observation**, verified live against
codex-cli 0.161.0 (`scripts/e2e-codex.sh`) and Claude Code (`scripts/e2e.sh`):

- **Codex edits through the shell** — every captured change was a command,
  once a Python heredoc (`docs/codex-hook-payload.md`). So `observe.rs`
  brackets tool calls instead of interpreting them: `PreToolUse` leaves a
  marker, `PostToolUse` reports every workspace file whose mtime falls between
  the marker's and now, as a `Write`. git bounds the walk where there is a
  repository; elsewhere hidden directories are skipped and it is capped.
  **The rule for every adapter:** structured tools are translated; anything
  that runs arbitrary commands is bracketed. Claude's `Bash` is too, so its
  `sed -i` edits are no longer invisible.
- Codex gives MCP servers a scrubbed environment, so `flag` cannot learn its
  session; every hook records `active-session` in the workspace directory and
  the server falls back to it. Latest wins.
- `thirdeye install -a codex <path> | --user` writes Codex's own config
  — merged, never overwritten: `hooks.json`, an `[mcp_servers.thirdeye]` block
  between markers in `config.toml`, the skill (`integration/codex.rs`).

Built — **OpenCode 2**, verified live (`scripts/e2e-opencode.sh`, against
2.0.24): no shell hooks exist, so `crates/thirdeye/agents/opencode/plugin.js` — loaded into
OpenCode's server, embedded in the binary — forwards raw events in an
envelope adding the turn and directory, and `opencode.rs` translates them:
`read`/`write`/`edit` (edits with their hunk range) translated, `shell`
bracketed. The plugin defines its own `thirdeye_flag` tool, whose `execute`
receives the exact session, and runs the new `thirdeye flag`. Facts:
`docs/opencode-plugin-events.md`.

Built — **pi**, verified live (`scripts/e2e-pi.sh`, against 1.1.0 on
`openai-codex/gpt-6-luna`): OpenCode's shape reused — `crates/thirdeye/agents/pi/extension.js`
forwards raw events in the same envelope, minting the turn id pi lacks, and
`pi.rs` translates. Unlike OpenCode's plugin, pi's extension runs in the
agent's own process, so surfaces work unchanged. `patch.rs` holds the hunk
parser both share, and `integration/plugin_files.rs` the install logic. Facts:
`docs/pi-extension-events.md`.

Built — **GitHub Copilot CLI**, verified live (`scripts/e2e-copilot.sh`,
against 1.0.94): its payloads name **neither their event, nor the turn, nor
the call** (`docs/copilot-hook-payload.md`). So each hook is registered with
`thirdeye hook --agent copilot --event <name>`, which wraps the payload in
OpenCode's envelope; the hook keeps the open turn itself (`held_turn.rs`,
from the edges `Payload::turn` marks — Copilot's hooks are synchronous, so
the order holds); and a bracket is keyed by a hash of the tool and its
arguments, under which observed writes get no dedup key, since every run of
a command shares it. Its tool set follows the model, so any tool the adapter
does not know is bracketed. Plugins are per user: `thirdeye install --user
-a copilot` writes a local marketplace, which Copilot loads live rather than
copying. Its MCP server starts in the plugin's directory, so `flag` finds the
workspace through the session's pointer.

Built — **Antigravity CLI** (`agy`), verified live (`scripts/e2e-agy.sh`,
against 1.3.2): Copilot's shape again — `--event`, a held turn — but with a
real call id (`stepIdx`), no prompt text and **no session end**. So the hook
records the agent's process in the session pointer (`agent_process.rs`: the
hook's first non-shell ancestor, with its start time against pid reuse) and
the daemon polls it, ending the session itself when it is gone — which also
reaps the daemon of a crashed Claude Code. `thirdeye install -a antigravity`
merges into Antigravity's own files. Facts: `docs/antigravity-hook-payload.md`.

Built — **Grok Build**, verified live (`scripts/e2e-grok.sh`, against
1.0.50): its payloads name their event and carry a real call id
(`toolUseId`), but — against its own docs — **tool events carry no turn id**,
so the hook holds the turn between `UserPromptSubmit` and the `Stop` that
names it, as for Copilot, under Grok's `promptId`. Structured tools are
translated (`search_replace` with the lines it now occupies); a read of a
missing file is still a `PostToolUse` and is told apart by its result's
shape. Per user: `thirdeye install --user -a grok` has Grok install a plugin,
which Grok **copies**. Its tool events were captured on another provider's
model through a custom-model entry; the harness, not the model, shapes them.
Facts: `docs/grok-hook-payload.md`.

Built — **a window where the terminal cannot split** (roadmap phase 3,
`docs/windows.md`), verified live against Ghostty 1.3.1, Alacritty 0.13.2 and
GNOME Terminal 3.52: `ghostty.rs`, `alacritty.rs`, `gnome_terminal.rs`, with
what they share in `window.rs`. Such a surface is `detached()`: it can open a
window but not close one, so **the daemon closes it by sending `Close` to the
head the surface launched** (roadmap 0.1) — the head exits cleanly and the
terminal closes the window; that head's leaving finds the surface closed and
records no second hide. And a detached surface's session starts in the
`attention` policy: a window over the agent is not a split beside it.

Built — the **Zellij surface**, `daemon/surface/zellij.rs`, verified live
(`docs/zellij.md`, against 0.45.1): `new-pane --no-focus` from outside the
session, which places the pane beside the agent only when the call carries
the agent's `ZELLIJ_PANE_ID` in its environment, so it runs through `env`.
`--close-on-exit`, or a head that exits leaves a dead pane. And one
**split size** for every surface, `THIRDEYE_SPLIT` (`surface::split_percent`,
default 40): tmux and WezTerm take it as is, Zellij in 5% `resize` steps from
its even split, kitty not at all — its `--bias` means something different in
every layout.

Built — the **WezTerm surface**, `daemon/surface/wezterm.rs`, verified live
(`docs/wezterm.md`): one file and one registry line. It reaches the GUI
through the recorded `$WEZTERM_UNIX_SOCKET`, passes `--no-auto-start` on every
call (without it `wezterm cli` spawns a mux server when no GUI answers), and
hands focus back after `split-pane`, which takes it with no flag to prevent
that — to the pane active in the agent's tab, not the agent's pane.

Built — the **Windows Terminal surface**, from WSL,
`daemon/surface/windows_terminal.rs`, verified live
(`docs/windows-terminal.md`, against 1.24 on WSL 3.0): a **window**, through
`window.rs`, although Windows Terminal can split — `wt.exe split-pane` splits
whichever pane has focus, in whatever tab or window that is, so a split would
follow the human rather than the agent. `wt.exe` parses its arguments again:
a `;` is escaped and a `"` refused. The same WSL run found that the daemon's
`PATH` is not the agent's (see Gotchas).

Built — **Windows itself**, for Claude Code in Windows Terminal, verified
live (`docs/native-windows.md`, against Windows 10 19045, by two `claude -p`
sessions): the workspace builds and passes its gates on
`x86_64-pc-windows-msvc`. There is no service manager, so **the hook starts
the daemon and `SessionEnd` ends it** with an `End` message
(`notify.rs`); `SelfBind::claim` keeps one of the several a turn's hooks
start. The daemon is started in two steps (`daemon --detach`), because a
process std starts inherits the hook's pipes and whoever ran the hook waits
on them. `local_socket.rs` is the one place that says which Unix socket
type a platform has; `agent_process.rs` reads Windows' process list where
Linux has `/proc`. State is under `%LOCALAPPDATA%\thirdeye`. `windows_terminal.rs`
serves both sides of Windows, told apart by what the build is, not by the
environment. **Zellij and WezTerm** work there unchanged in behaviour
(verified through `thirdeye hook`, 2026-10-15): a variable set for one call
goes through `Runner::run_with_env`, never an `env` program, and the runner
finds `zellij` as `zellij.exe`.

Built — **installation** (`docs/design/plans/2026-10-15-installation.md`),
verified here; the Windows half and the first release are not:

- `integration/` — the installer's agent seam, moved into the binary from
  xtask: one file per agent implementing `Integration`. **Projects point,
  thirdeye's directory holds**: `publish` writes everything that changes
  between versions into `$XDG_DATA_HOME/thirdeye/<agent>/`, and `install`
  writes into the project only what stays the same — the binary's path, the
  directory's path, a symlinked skill (`skills.rs`), a stub that imports the
  real script (`plugin_files.rs`). A test per agent installs, publishes
  again, and asserts the project byte for byte unchanged. An update never
  walks a list of projects: it rewrites that directory. **Nor does anything
  else — thirdeye keeps no list of projects at all.** Every command names
  its place the same way, a project's path or `--user`, and its agents as
  `-a <id>[:<config dir>]`; `self uninstall` warns that it cannot unwire
  projects. `<id>:<dir>` sets the variable the agent itself reads for its
  configuration directory (`Integration::config_var`) — for its CLI and
  every path derived — through the context, never `set_var`.
- Claude Code installs from embedded files, as a local marketplace in that
  directory, which Claude **loads live** (`readFromFolder`, verified against
  2.1.300) — so its old stale-cache problem is gone.
- `thirdeye install | uninstall | refresh` (`cmd_install.rs`) — `install`
  naming no agent finds the ones on this machine and asks about each, and
  installs the units if missing — and
  `published.toml` in thirdeye's directory (`published.rs`): which build
  wrote it, from which binary, and what installed that binary. The hook
  compares the build and republishes a directory another build wrote, but
  only from the binary named there; `self update` reads who owns it.
- `install.sh` / `install.ps1` at the root, `thirdeye self update |
  uninstall` (`cmd_self.rs`), `.github/workflows/{ci,release}.yml`.
  `scripts/test-install.sh` tests the script, self update and self uninstall
  against a locally served archive.

Not built yet: transcript tailer.

**Next:** the roadmap in `README.md` — the rest of Windows (other agents,
Alacritty) and macOS. Every fact an adapter or surface rests on is
captured live first, as `docs/hook-payload.md` was.

The design and its rationale live in `docs/design/specs/`, and the plans
that implement them in `docs/design/plans/`. A plan records how it was
carried out; it does not edit the spec.

## Commands

```bash
cargo xtask configure                                   # probe this machine, write install.toml
cargo xtask install                                     # build + install exactly what it says
thirdeye doctor <path>… | --user                        # what is installed, and does it agree
thirdeye install <path> | --user                        # what a user runs: find agents, ask, wire
thirdeye install -a <id>[:<dir>]… <path> | --user       # the scriptable form; uninstall alike

cargo test --workspace                                  # full suite
cargo test -p thirdeye-core <name> -- --nocapture       # one test
cargo clippy --workspace --all-targets -- -D warnings   # lint gate
cargo fmt --check                                       # format gate

scripts/test-install.sh                                 # install.sh + self update against a local archive; free
scripts/e2e.sh                                          # real agent + kitty; costs SDK credit
scripts/e2e-codex.sh                                    # the same for Codex; costs OpenAI credit
scripts/e2e-opencode.sh                                 # the same for OpenCode; free model by default
scripts/e2e-pi.sh                                       # the same for pi; costs OpenAI credit
scripts/e2e-copilot.sh                                  # the same for Copilot; costs AI credits
scripts/e2e-agy.sh                                      # the same for Antigravity
scripts/e2e-grok.sh                                     # the same for Grok Build
```

**Never rebuild only the binary.** What the agents run is the *installed*
binary, and the files in thirdeye's directory name it; a build left in
`target/` is neither. Milestone 7 lost an afternoon to a binary eighteen
minutes stale behind a current plugin. `cargo xtask install` installs the
binary, has it republish every integration and the units, and then checks.

Two rules the installer must keep, both learned by getting them wrong:
**no agent is wired unless the developer names it** (a default target bakes one
machine's layout into the repo), and **agents live behind a trait**
(`crates/thirdeye/src/integration/`) because §5 makes them a seam — an
installer hardcoding `claude` contradicts the architecture it installs. xtask
now only decides which agents and projects; the wiring is the binary's.
`docs/development.md` is the full story.

All four must pass. `clippy::all` is `deny` and `unsafe_code` is `forbid` at
the workspace level, so lint failures are build failures, not warnings.

## Architecture

Producers and consumers never talk to each other. They meet at an append-only
event log on disk:

```
adapters ──► FileEvent ──► log segments ──► TrackerState ──► heads
```

**The log is the drop point, never a daemon.** Agent hooks are short-lived
shell commands; they need somewhere to put events that exists whether or not
anything is watching. A file is that somewhere — no lifecycle, no port, no
events lost while the viewer is closed. A future GUI/web head tails the same
log.

There *is* a daemon now (milestone 4), and this rule is what constrains it:
**it owns no unique data.** Producers append before any daemon logic runs and
regardless of whether one is alive; everything the daemon holds is derived by
replaying the log. So it can be killed, crashed or orphaned without loss, and
"all events are collected regardless of viewer state" holds *structurally* —
no policy branch can skip the append, because the append happens first, in a
different process. Its every decision must therefore be a pure function of the
whole session rather than of what arrived while it happened to be running, or
a restarted daemon would disagree with one that never stopped.

### Where the seams live

§5 of the spec names five seams. Each has one directory; **`mod.rs` holds the
seam and nothing else, and every implementation is a file named after itself.**
Adding one is adding a file and a `pub mod` line, never editing an
implementation someone else relies on.

| Seam | Directory | Today | A second one is |
|---|---|---|---|
| Adapter | `crates/thirdeye-adapters/src/` | `claude.rs`, `codex.rs`, `opencode.rs`, `pi.rs`, `copilot.rs`, `antigravity.rs`, `grok.rs` | `goose.rs` |
| Head | `crates/thirdeye/src/heads/` | `tui/` | `gui/`, or a separate program |
| Surface | `crates/thirdeye/src/daemon/surface/` | `tmux.rs`, `zellij.rs`, `wezterm.rs`, `kitty.rs`, `ghostty.rs`, `alacritty.rs`, `gnome_terminal.rs`, `windows_terminal.rs`, `null.rs` | `konsole.rs` |
| Transport | `crates/thirdeye/src/daemon/transport/` | `io.rs`, `memory.rs` | `websocket.rs` |
| Activation | `crates/thirdeye/src/daemon/activation/` | `systemd.rs` (Unix only), `self_bind.rs` | `launchd.rs` |
| Integration | `crates/thirdeye/src/integration/` | `claude.rs`, `codex.rs`, `opencode.rs`, `pi.rs`, `copilot.rs`, `antigravity.rs`, `grok.rs`, `unsupported.rs` | `goose.rs` |

Two of these are not Rust traits, and pretending otherwise would mislead:

- **A head is whatever speaks the socket protocol.** Not a trait, not
  necessarily this crate or this language — that is why the protocol is
  newline-delimited JSON. `heads/` holds the ones that ship with thirdeye.
- **The installer has its own agent seam** (`integration/`, the sixth row),
  separate from the adapter: one is how thirdeye *reads* an agent, the other
  is how it *installs into* one. An agent needs both before it works. (It is
  a trait; it is listed here because it is not one of the spec's five.)

**Adapters are chosen by the caller, not by the payload.** The installed hook
runs `thirdeye hook --agent <id>`. Claude Code's and Codex's payloads are near
copies of each other, so sniffing the shape would file one agent's events as
the other's without a single error.

Two rules hold the seam together. Most design mistakes here are violations of
one of them:

1. **Adapters translate, heads consume.** No head ever parses an
   agent-specific format. Adding a head must not touch adapter code; adding an
   agent must not touch head code.
2. **`TrackerState` is per-session.** Which session a head attaches to is a
   head concern, kept out of the core entirely.

### Log layout

```
$THIRDEYE_STATE_DIR | $XDG_STATE_HOME/thirdeye | ~/.local/state/thirdeye
  <workspace-slug>/          # basename + FNV-1a hash of the absolute path
    <session-id>/            # or `_unattributed`
      <producer>.jsonl
```

- **One segment per producer, not one shared log**, because unrelated
  producers have unrelated lifetimes; `read_session` merges them by timestamp.
  This does *not* remove the append-atomicity assumption, only its
  cross-producer half: the plugin's hooks run `"async": true`, so many
  concurrent `thirdeye hook` processes append to one `hook.jsonl`. Atomicity
  for them rests entirely on `MAX_LINE_BYTES` (4096) staying at or below
  `PIPE_BUF`, which is why `Writer::append` *enforces* the cap against the
  encoded line — clipping fields, then the path, then refusing the record —
  rather than making a best effort at it.
- **State is owner-only** (`0700` dirs, `0600` segments). The log holds full
  prompt text and every path the agent touched; the default umask would leave
  it world-readable.
- **Session partitioning is structural**, not cosmetic. Every reader assumes
  it; flattening it would be a rewrite.
- Unattributable events go to `_unattributed` rather than being dropped, so a
  broken adapter is visible instead of silently invisible.
- Session ids and workspace names are **untrusted** for path purposes —
  everything goes through `sanitize_component`.

### Dedup

A hook and a transcript tailer both report the same tool call. Claude Code
exposes `tool_use_id` on *both* paths (verified — `docs/hook-payload.md`), so
dedup is an exact set-membership test on `dedup_key`, not a timing heuristic.
Push sources (`Hook`, `Mcp`) beat pull sources (`Transcript`, `Sqlite`) on
ties. Events without a `dedup_key` are never collapsed — two genuine reads of
one file must both count toward ranking.

### Turns

`thirdeye ls` groups records by `prompt_id`, opened by `UserPromptSubmit` and
closed by `Stop`. Grouping is keyed by id, never by position: hooks are async,
so a tool-use record can be written before the marker that opens its turn, and
installing thirdeye mid-session means the opening marker is simply missing.
Both must degrade rather than lose events.

Unrecognized `record` tags and newer format versions deserialize to
`LogRecord::Unknown` and are counted apart from malformed lines. A stale binary
reading a log a newer one wrote is routine — a session's daemon keeps running
the binary it started with across an update — and
reporting that as corruption would send someone hunting a bug that isn't there.

### Ranking

Score is a recency-decayed sum of event weights (mutation > read > reference),
kept as an O(1) running total plus a `score_ts`, decayed forward on demand.
That identity — decaying a running total equals summing individually-decayed
weights — is what makes the O(1) representation legitimate, and it is pinned
by a test. If you change the decay function, that test must still hold.

### Staleness

A file that changed *after* the agent last read it is the thing a human wants
flagged. Detected by comparing on-disk mtime against `last_content_seen`, with
`STALENESS_GRACE_MS` absorbing the agent's own writes. An unreadable or
deleted file is `Unknown`, never `Fresh` — absence of evidence must not render
as a clean bill of health.

## Conventions

- Rust 2024 edition, workspace-inherited `[lints]` — new crates need
  `[lints] workspace = true`.
- Tests are colocated in `#[cfg(test)] mod tests` at the bottom of each module.
- Test names state the property being held, not the function being called
  (`push_wins_over_pull_for_the_same_tool_call`, not `test_dedup`).
- Comments explain *why* a choice was made over the obvious alternative;
  they do not restate the code.
- Any new indicator gets a **perturbation test** — prove it goes red on a
  planted defect before trusting its green. See
  `staleness_detects_a_real_on_disk_change`.

## Gotchas

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
- **The daemon's `PATH` is not the human's.** Under systemd it is the user
  manager's, which holds `~/.local/bin` or `~/.cargo/bin` only if a login
  session exported them — never on WSL. So `PATH` is in `SURFACE_ENV`, the
  `ProcessRunner` searches and runs with the agent's, and every surface
  launches the head by the daemon's own full path. A bare program name
  anywhere on the daemon's side is this bug again.
- **Nothing may call `std::env::set_var`.** It is `unsafe` in Rust 2024 and the
  workspace forbids `unsafe`. Anything whose behaviour depends on the
  environment takes a lookup closure instead, which is why `surface::capture`
  has the signature it does.
- **A daemon ends its own session only when the agent's process is gone.**
  It polls the pid the hook recorded every 5 s, and on its own way out does
  what `SessionEnd`'s hook does, then exits. A pointer naming no process
  (another platform, an older hook) ends nothing.
- **Nothing runs on the daemon's way out under systemd.** `systemctl stop` is
  SIGTERM, and with `unsafe` forbidden there is no handler; the cleanup after
  `serve` only runs when the listener closes on its own. Anything that must
  happen at session end belongs in the hook's `SessionEnd` path or in the
  head noticing its connection drop.
- **thirdeye is a binary crate, so unused code is a lint failure.** A plan
  that lands a type in one task and its first caller in a later one cannot
  keep every task green; commit them together rather than adding
  `allow(dead_code)`.

- **Observed writes are windowed on change time** — the later of mtime and
  ctime, so `cp -p`, `tar x` and `mv` are seen. A file the human saves while
  an agent's command runs is reported as the agent's write, and a structured
  edit made during a `Bash` call is counted twice; a window stops where the
  next call began, so sequential commands are not. **Deletions** are
  `Delete` events: a call's marker snapshots which files the session knows
  exist and which tracked files git already misses, and the difference at
  the end is what the call deleted. A file in neither — never touched,
  untracked or outside a repository — is deleted unseen.
  Claude reports a failing command through `PostToolUseFailure`, which closes
  its window like `PostToolUse`.
- **An agent loading a skill is not a read.** Codex `cat`s a skill's
  `SKILL.md` (its own prompt says to open the listed path) and pi reads it
  with `read`; `translate` drops reads under any agent's skills directory
  (`adapters::is_skill_file`), uniformly. Edits there stay visible.
- **Codex skips hooks until they are trusted in `/hooks`,** and trust is keyed
  on a hash of each hook's definition — every install that changes
  `hooks.json` needs re-approval, or thirdeye silently records nothing. It
  also asks before every MCP call unless told otherwise, which is why the
  installed config sets `approval_mode = "approve"` for `flag` alone.
- **Other agents run Claude's hooks.** Grok Build and Copilot CLI both
  execute the hooks in `.claude/settings.json`, with payloads that repeat
  enough of Claude's fields for the Claude adapter to accept them. The Claude
  path stands aside when a variable only their hook runners set is present
  (`CLAUDE_HOOK_BORROWERS` in `cmd_hook.rs`) — never one their shell tools
  pass on, or a Claude Code session started from them would go silent.
  Grok runs no Claude *plugin* hooks. **Copilot loads Claude's plugins** that
  a project's `.claude/settings*.json` enables, in an interactive session —
  so in a project wired for Claude it runs thirdeye's Claude plugin, whose
  hooks stand aside and whose MCP server lists no tool when
  `COPILOT_PLUGIN_ROOT` is set (`CLAUDE_SERVER_BORROWERS` in `cmd_mcp.rs`;
  `COPILOT_AGENT_SESSION_ID` reaches Copilot's shell commands too, so it
  would silence a Claude Code started from one). A user-installed Copilot
  plugin of the same name wins over it (`docs/copilot-hook-payload.md`).
- **`thirdeye mcp` reads each agent's own session variable**, never
  another's: an agent started inside Claude Code inherits
  `CLAUDE_CODE_SESSION_ID`, and its flags would land in Claude's session.
- **Every install pre-approves the `flag` tool where the agent lets it**
  (`docs/install.md`, "Flags without a prompt"): most agents ask before an
  MCP tool's first use, and a flag that waits arrives late. A new agent's
  installer must find that setting too, and its uninstall must remove it.
- **A hook's stdout is not always ignored.** Grok reads it as a decision
  (`PreToolUse`, `Stop`) or as feedback for the model (`PostToolUse`), and
  Antigravity denies a tool call whose `PreToolUse` hook prints an object
  without a `decision`. Their installed commands send stdout to `/dev/null`.
- **OpenCode's plugin runs in its server, not in the terminal.** The default
  server is a shared background service, so the plugin sees *its*
  environment — no `TMUX`, no `KITTY_LISTEN_ON` — and the viewer cannot open
  itself; `opencode --standalone` inherits the terminal and works. The plugin
  is glue: it forwards raw events and interprets none, so `opencode.rs`
  remains the only place OpenCode's format is read.
- **Plugin and hook configuration is read at session start.** Enabling the
  plugin or editing `.claude/settings.json` does nothing to a running session;
  a fresh `claude` process is required. `docs/install.md` says so, and any
  future install command must too.
- **Editing `crates/thirdeye/agents/` changes nothing installed.** The
  templates are embedded in the binary; what agents load is what the
  installed binary published into `~/.local/share/thirdeye/`. `cargo xtask
  install` republishes. Claude Code reads that directory live; it still
  makes a copy in `~/.claude/plugins/cache/`, which is not what runs, and
  `claude plugin update` acts on any change of version string — so the
  manifest's version carries a hash of the files, for a Claude that would.
- **Never `claude plugin marketplace remove`, at any scope, for one
  project.** It forgets the marketplace for every project and unwires all of
  them (verified against 2.1.300). Claude's uninstall removes the plugin and
  that scope's `extraKnownMarketplaces` entry only.
- **A project's skill is a symbolic link** into thirdeye's directory (Codex,
  Antigravity, OpenCode, pi), so it updates with the directory — a link to
  the *directory*: Codex ignores a skill whose `SKILL.md` alone is a link.
  On Windows it is a copy, and `doctor` reports one an update left behind.
- **A running OpenCode server reloads its plugin** when the file the stub
  imports changes, so a republish reaches even the shared service.
- **`thirdeye mcp` must write nothing but JSON-RPC to stdout.** One stray
  `println!` desynchronizes the client and the server looks broken. This is
  the exact inverse of the hook path, where *stderr* is forbidden because a
  hook's stderr surfaces inside the agent session. Diagnostics go to stderr
  here, stdout there — do not copy a logging line from one to the other.
- **`tool_response` in a hook payload embeds the whole file content.** Discard
  it. The log must not become a second copy of the user's source tree, and
  lines must stay under `MAX_LINE_BYTES`.
- **Do not use `DefaultHasher` for anything persisted.** It is explicitly not
  stable across Rust releases; workspace slugs use hand-rolled FNV-1a.
- **Payload shapes in `docs/hook-payload.md` were captured from a live agent,
  not inferred.** The M0 spike that produced them is gone; that file is now the
  only record, so treat it as the source of truth and re-verify against a real
  session before changing an adapter's assumptions.
- **Both targets are a gate now.** Code behind `cfg(windows)` or
  `cfg(unix)` is invisible to the other's lints, and unused code is a build
  failure here. From either side: `rustup target add` the other
  (`x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`) and run `cargo
  clippy --workspace --all-targets --target <it> -- -D warnings`, which
  needs no linker.
- **On Windows, never start a long-lived process with `std::process`
  from a hook or the MCP server.** It inherits their pipes, and the agent
  waits on those: the hook looks hung long after it exited. `notify.rs`
  has the way that does not.
- **On Windows, nothing may be found through `HOME`, `TEMP` or `bash`.**
  A hook run through Git Bash has its own of the first two and the viewer
  the terminal starts has neither; and `bash` on `PATH` is WSL's as often
  as Git's. `thirdeye_core::home_dir`, `%LOCALAPPDATA%`, and a `.cmd`
  beside any script something other than Claude's hook runner starts.
- **Paths on Windows are text with backslashes.** A test that writes one
  into JSON must escape it, and `canonicalize` returns a `\\?\` spelling
  no agent uses — `workspace.rs` strips it, and nothing else may
  canonicalize a root. A canonical root has the disk's case and an agent
  names files in its working directory's, so `Writer::append` gives every
  path under the root the root's spelling (`Workspace::respell`): readers
  compare by component, which is case-sensitive, and must never have to
  fold case themselves.
- **`thirdeye hook` and `thirdeye ls` must resolve the workspace identically.**
  The slug is derived from path *text*, so a difference of spelling silently
  splits one project into two workspaces and `ls` reports "no sessions
  recorded" for a session that exists. Every command goes through
  `crates/thirdeye/src/workspace.rs`; nothing else may compute a root.
