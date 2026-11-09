# thirdeye PoC — design

**Date:** 2026-08-01
**Status:** approved, ready for implementation planning
**Supersedes:** the M2–M7 sections of the earlier, unpublished project plan.
M0 (hook payload verification) and M1 (`thirdeye-core`) are already built and remain valid.

---

## 1. Context

When you drive a CLI coding agent, it reads, greps and edits a shifting set of
files. You cannot see that set. You scroll the transcript to find what it just
touched, or keep a second editor open and hunt manually.

**thirdeye** watches the agent and shows the human which files are in play —
grouped by conversational turn, badged with what happened to each, prioritized
by files the agent explicitly asked you to look at, and opened at the line
range the agent actually touched.

The PoC targets Claude Code in a kitty terminal. That combination must not
leak into the architecture: agents, heads, surfaces, transports and activation
strategies are each independently pluggable seams.

---

## 2. Scope

**In scope for the PoC**

- A Claude Code plugin providing hooks, an MCP server, a skill, and two slash
  commands.
- A per-session background daemon holding all UI logic.
- A thin ratatui head rendering in a kitty split.
- Turn-grouped file list; full-pane content/diff on selection.
- An MCP tool letting Claude flag files for human attention.
- Configurable reopen policy, adjustable from inside the viewer.

**Explicitly out of scope for the PoC** (seams defined, implementations deferred)

- Agents other than Claude Code.
- Heads other than the TUI.
- Surfaces other than kitty.
- Transports other than unix sockets.
- Activation strategies other than systemd socket activation.
- Dynamic plugin discovery/loading — the PoC uses a static registry behind the
  same traits and manifest shapes.

**Non-goals**

- Editing files from the viewer. It is a viewer.
- Replacing the agent transcript.
- Cross-session or cross-workspace aggregate views.

---

## 3. Verified environment facts

Everything below was checked on the target machine, not assumed.

| Fact | Evidence |
|---|---|
| Rust 1.94.0 / cargo 1.94.0 | `rustc --version` |
| All nine hook events exist, incl. `SessionEnd` and `Stop` | symbol extraction from the `claude` binary |
| `PostToolUse` provides `session_id`, `cwd`, `prompt_id`, `tool_name`, `tool_input`, `tool_response`, `tool_use_id` | captured live; see `docs/hook-payload.md` |
| **`Read`** `tool_response.file` carries `startLine` / `numLines` / `totalLines` | same capture |
| **`Edit`** `tool_response` has **no `file` object** — it carries `structuredPatch[]` of `{oldStart, oldLines, newStart, newLines, lines}` | later live capture, 2026-08-01 |
| `UserPromptSubmit` provides `prompt` + `prompt_id` | same capture |
| Project `.claude/settings.json` is read at **session start** only | spike produced nothing until a fresh `claude` ran |
| kitty 0.48.0, `allow_remote_control socket-only`, `listen_on unix:/tmp/kitty-{kitty_pid}` | `kitty --version`, `kitty.conf` |
| `kitten @ launch` supports `--type`, `--cwd`, `--title`, `--keep-focus` | `kitten @ launch --help` |
| `kitten @ close-window --match` supports `env:` matching | `kitten @ close-window --help` |
| systemd 255, user manager running, `Linger=yes` | `systemctl --user is-system-running`, `loginctl` |
| `XDG_RUNTIME_DIR=/run/user/1000`, mode `0700`, user-owned | `ls -ld` |
| Plugin layout: `.claude-plugin/plugin.json` + `hooks/hooks.json` via `${CLAUDE_PLUGIN_ROOT}` | inspected an existing plugin's layout |

**Two measured constraints that shaped the design:**

- A socket at `<session-dir>/daemon.sock` is **111 bytes** against a 108-byte
  `sun_path` limit on Linux (127 vs 104 on macOS). It would not bind. Sockets
  therefore live in `$XDG_RUNTIME_DIR`, measured at **65 bytes** — 39 bytes of
  headroom even against the stricter macOS limit.
- The current terminal is ghostty, not kitty. kitty is installed and
  configured, but the PoC requires running Claude Code *inside* kitty.

---

## 4. Architecture

### 4.1 Process topology

```
Claude Code
  │
  ├─ hooks ──────► thirdeye hook          (ephemeral, per tool call)
  │                    │ 1. append event to log      ← durable, unconditional
  │                    └─ 2. connect to socket, write one advisory line, close
  │                              │
  │                              └─► systemd socket-activates the daemon
  │
  └─ MCP ────────► thirdeye mcp           (per session, Claude Code owns lifetime)
                       └─ flag tool → appends event to log

                   thirdeye daemon         (per session, socket-activated)
                       │ tails log → model, turn grouping, ranking
                       │ owns policy + durable view state
                       │ decides visibility, drives Surface
                       └─ serves heads over the same socket
                              │
                              └─► thirdeye head tui   (inside a Surface)
```

### 4.2 The load-bearing invariant

**The daemon owns no unique data.** Hooks append to the append-only log
*before* any daemon logic runs and regardless of whether a daemon is alive.
The daemon only ever tails that log; everything it holds is derived state.

Consequences:

- The daemon can be killed, crashed or orphaned without data loss. The next
  hook reactivates it and it replays the log to identical state.
- "All events are collected regardless of viewer state" holds *structurally* —
  no policy branch can skip the append, because the append happens first, in a
  different process.
- The socket notification is **advisory, never the data path**. The daemon
  learns *that* something happened from the socket and *what* happened from
  the log. A failed connect costs latency and nothing else.

Durable view state (selection, expanded turns) is the one exception, and is
written through to `viewstate.json` rather than held in memory alone.

### 4.3 Model / presentation boundary

The daemon sends **semantic** data; the head decides **presentation**.
Syntax highlighting, colour, glyphs and layout are head concerns — a web head
highlights differently than ratatui, and neither should be dictated by the
daemon.

The daemon owns the model *and* durable view state. The head owns rendering,
key mapping, and size-dependent ephemera such as scroll offset. Pushing scroll
position into the daemon would reduce heads to dumb framebuffers and force
terminal interaction semantics onto a future web head.

The daemon serves file content rather than letting the head read the disk.
Slightly more traffic, but it keeps heads thin, keeps git-diff knowledge in one
place, and is the difference between a remote head being possible and being a
rewrite.

---

## 5. The five seams

| Seam | Boundary | PoC | Deferred |
|---|---|---|---|
| Adapter | the log format | Claude Code hook + MCP | Codex, Cursor |
| Head | socket protocol | ratatui TUI | GUI, web |
| Surface | `Surface` trait | kitty | tmux, wezterm, standalone window |
| Transport | endpoint descriptor | unix socket | named pipe, loopback TCP |
| Activation | `Activation` trait | systemd socket activation | launchd, self-spawn + file lock |

**Surface and Head are separate axes.** kitty is *where* a head is displayed;
TUI is *what* the head is. A GUI head makes its own window and needs no
Surface at all.

Heads and surfaces are separately-versioned plugins resolved by **capability
matching**, not hard-wired pairs:

```toml
[head]  id = "tui"   protocol = 1  requires_surface = "embeds-terminal-program"
[head]  id = "gui"   protocol = 1  requires_surface = "none"

[surface] id = "kitty"  provides = "embeds-terminal-program"
[surface] id = "tmux"   provides = "embeds-terminal-program"
```

The daemon resolves `head → required capability → an available surface
providing it`, failing with a concrete message when nothing matches (e.g.
"head `tui` needs a terminal surface; kitty not detected — `KITTY_LISTEN_ON`
unset").

```rust
trait Surface {
    fn provides(&self) -> SurfaceCapability;
    fn available(&self) -> bool;
    fn show(&self, spec: &HeadSpec) -> Result<SurfaceHandle>;
    fn is_alive(&self, h: &SurfaceHandle) -> bool;
    fn hide(&self, h: &SurfaceHandle) -> Result<()>;
}

trait Transport {
    fn available() -> bool;
    fn bind(&self, id: &SessionKey) -> Result<(Listener, EndpointDescriptor)>;
    fn connect(desc: &EndpointDescriptor) -> Result<Stream>;
}
```

---

## 6. Activation and lifecycle

### 6.1 Why socket activation

Socket activation starts a service when a client connects. The daemon must run
with *no head connected* — it is what decides to open the head. Pure socket
activation does not fit that shape until **the hook becomes the client**, which
it does here.

On systemd this *removes* code: `flock` liveness, detached spawning, orphan
reaping and restart logic all become systemd's responsibility. Single-instance
is guaranteed per template instance, the cgroup cleans up, idle-exit is free
because the next hook reactivates, and logs go to the journal.

```ini
# ~/.config/systemd/user/thirdeye@.socket
[Socket]
ListenStream=%t/thirdeye/%i.sock     # %t = XDG_RUNTIME_DIR
SocketMode=0600
RemoveOnStop=yes

# ~/.config/systemd/user/thirdeye@.service
[Service]
Type=notify
ExecStart=%h/.local/bin/thirdeye daemon --session %i
```

- `Accept=no` (the default): the *listener* is passed and the daemon accepts.
  `Accept=yes` would spawn a process per connection, which is wrong here.
- Hook and head share one socket; `Hello { role: Hook | Head }` disambiguates,
  so one unit pair covers both.
- Session ids are agent-supplied and must go through `systemd-escape` before
  becoming instance names.

### 6.2 Hook wiring

| Hook event | Action |
|---|---|
| `SessionStart` | `systemctl --user start thirdeye@<esc>.socket`; record `KITTY_LISTEN_ON` and `cwd` into the session dir, since only the hook environment has them |
| `UserPromptSubmit` | append turn-open marker (`prompt_id`, `@`-mentions); clear a `not-this-turn` policy |
| `PostToolUse` | append file events; notify socket |
| `Stop` | append turn-close marker |
| `SessionEnd` | `systemctl --user stop thirdeye@<esc>.socket` |

Hooks never touch the terminal. All terminal manipulation belongs to the
daemon, via `Surface`. This keeps the hot path — which runs on every mutation —
to an append plus a short connect.

If a `PostToolUse` connect fails with `ENOENT` (socket unit not started), the
hook starts the unit and retries once. The event is already durable at that
point, so even a total failure is latency-only.

### 6.3 Non-systemd fallback (deferred, but real)

Alpine/musl, containers and WSL without systemd are real Linux configurations.
The fallback is self-spawn plus a cross-platform file lock (`fs4` or an
equivalent `ProcessLock` abstraction — `flock` itself is POSIX-only). Not
needed for the PoC; the seam exists so it is additive.

---

## 7. Storage layout

```
$XDG_STATE_HOME/thirdeye/<workspace-slug>/<session-id>/
  hook.jsonl       # events from the Claude Code hook adapter
  mcp.jsonl        # flag events from the MCP server
  policy.json      # reopen mode; written by head, read by daemon
  viewstate.json   # selection + expanded turns; survives head restarts
  endpoint.json    # how to reach the daemon (transport descriptor)

$XDG_RUNTIME_DIR/thirdeye/
  <session-id>.sock     # 0700 dir, 0600 socket, 65-byte path
```

One segment file per producer, never a shared log: concurrent `O_APPEND` is
only atomic below `PIPE_BUF`. Already implemented in `thirdeye-core`.

Discovery is decoupled from transport: the head reads `endpoint.json` and
learns how to connect.

```jsonc
{ "transport": "unix", "path": "/run/user/1000/thirdeye/<id>.sock" }
{ "transport": "pipe", "name": "\\\\.\\pipe\\thirdeye-<id>" }
{ "transport": "tcp",  "port": 54321, "token": "…" }
```

### XDG mapping

| Data | Location |
|---|---|
| Event logs, policy, view state | `$XDG_STATE_HOME/thirdeye/` |
| Sockets, endpoint descriptors | `$XDG_RUNTIME_DIR/thirdeye/` |
| Config | `$XDG_CONFIG_HOME/thirdeye/` |
| systemd units | `$XDG_CONFIG_HOME/systemd/user/` |

`EventPaths::discover` already honours `$XDG_STATE_HOME` with a `~/.local/state`
fallback. It gains a runtime-dir sibling now, and macOS/Windows branches later
(`directories` crate).

---

## 8. Rootless requirements

Hard requirement, not a preference:

- User systemd units only (`~/.config/systemd/user/`). Never
  `/etc/systemd/system`, never `systemctl` without `--user`.
- No privileged ports. The TCP fallback binds loopback on an ephemeral port
  with a token file, because any local process can reach loopback.
- No writes outside `$HOME` and `$XDG_RUNTIME_DIR`.
- No setuid, no capabilities, no `sudo` anywhere in install or runtime.
- Plugin install touches only user directories; uninstall is a file removal.

---

## 9. Data model

Extends the `FileEvent` already implemented in `thirdeye-core`.

A turn marker has no file path, so it cannot be a `FileEvent` — that type's
invariant is that every event names a file. The log therefore holds a tagged
record, of which file events are one arm:

```rust
#[serde(tag = "record", rename_all = "snake_case")]
enum LogRecord {
    File(FileEvent),
    Turn(TurnMarker),
}

struct TurnMarker {
    ts: Timestamp,
    session: Option<String>,
    prompt_id: String,
    kind: TurnKind,          // Open { prompt: String } | Close
}

// FileEvent::kind gains one variant; the rest are already implemented.
enum EventKind {
    Read { range: Option<LineRange> },
    Edit { range: Option<LineRange> },
    Write,
    Mention,
    Search { pattern: String },
    Exec { cmd: String },
    Flag { reason: String, range: Option<LineRange> },   // new
}
```

This is a breaking change to the log format, which costs nothing — nothing has
shipped — and is preferable to weakening `FileEvent` by making `path` optional
just to accommodate two pathless variants.

**Flags stay an event kind on `FileEvent`**, because a flag always names a
file. Turn grouping, dedup, ordering and replay therefore work on flags
unchanged, with no separate store.

**Range extraction is per-tool-family, not uniform.** An earlier revision of
this spec generalized a `Read`-only capture to every tool. A later live capture
disproved it — after the adapter had already been written against the wrong
assumption, with every test passing because no test payload included a
`tool_response`:

| Tool | Source of range |
|---|---|
| `Read` | `tool_response.file.startLine` + `numLines` |
| `Edit` / `MultiEdit` | `tool_response.structuredPatch[]` — span min `newStart` to max `newStart + newLines` |
| `Write`, `NotebookEdit` | none; no range |

The lesson generalizes past this one field: **a payload shape verified for one
tool says nothing about the others.** Any future adapter must capture the
specific tool it targets rather than extrapolating from a sibling.

**Turns** come from `prompt_id`, opened by `UserPromptSubmit` and closed by
`Stop`. A file touched in three turns appears in all three — the turn view is a
history, not a deduplicated set. `prompt_id` is load-bearing; an adapter that
cannot supply a turn id degrades to a single-group view.

**Dedup** is exact set-membership on `tool_use_id`, verified present on both
the hook and transcript paths. Push sources (`Hook`, `Mcp`) beat pull sources
on ties. Events without a dedup key are never collapsed.

---

## 10. Viewer behaviour

### 10.1 Layout

Turn-grouped list fills the pane; selecting a file swaps to a full-pane
content/diff view; Esc returns. Only one thing shows at a time, so nothing is
cramped at half-width.

```
┌─ thirdeye ─────────────┐   ┌─ src/log.rs ───────────┐
│ ▼ turn 3 (current)     │   │  88 │ fn dedup(...) {   │
│  ★ src/log.rs    E2 R1 │──►│  89 │-  let mut seen   │
│    src/main.rs   E1    │   │  90 │+  let mut chosen │
│ ▶ turn 2               │   │  91 │   for ev in ...  │
│ ▶ turn 1               │   │                        │
└────────────────────────┘   └────────── esc: back ───┘
```

Flagged files sort to the top of their turn, marked `★`, with the reason
rendered beneath and the range used as the jump target.

**Default detail mode:** diff for files the agent mutated, content for files it
only read or flagged. The user can toggle either way; the default just matches
the reason the file is on the list.

### 10.2 Visibility triggers

The split opens on **a mutation (`Edit`/`Write`) or an explicit flag**. Reads
alone do not open it — reading is constant and cheap; changes and flags are
what deserve attention.

Triggering and display are independent: read files **are** collected, ranked
and listed like everything else. They simply never cause the viewer to appear.

### 10.3 Reopen policy

Set from inside the viewer, stored in `policy.json`:

| Mode | Behaviour |
|---|---|
| `any` | any trigger reopens |
| `attention` | only explicit flags reopen |
| `not-this-turn` | suppressed until the next `UserPromptSubmit` |
| `never` | only the slash commands reopen it |

Events are collected in every mode. `/thirdeye-open` and `/thirdeye-close`
bypass policy entirely.

---

## 11. Head protocol

Newline-delimited JSON over the transport — debuggable with `nc`, and
language-agnostic for non-Rust heads.

| Direction | Message | Notes |
|---|---|---|
| → daemon | `Hello { role, head_id, protocol_version }` | daemon rejects incompatible versions |
| ← daemon | `Snapshot { turns[], flags[], policy, view_state }` | full state on connect |
| ← daemon | `Patch { turn_id, upserted[], removed[], policy?, view_state? }` | incremental thereafter; file entries are upserted whole, since they are small |
| → daemon | `Select { path }`, `ExpandTurn { id, bool }` | mutate durable view state |
| → daemon | `RequestDetail { path, mode: Content \| Diff }` | |
| ← daemon | `Detail { lines[], diff_markers[], range }` | raw lines + markers, not styled text |
| → daemon | `SetPolicy { mode }` | |
| ← daemon | `Close { reason }` | teardown on `SessionEnd` |

---

## 12. kitty surface

- **Launch:** `kitten @ launch --type=window --keep-focus --cwd=<workspace>
  --title=thirdeye --env THIRDEYE_SESSION=<id> thirdeye head tui --session <id>`.
  `--keep-focus` means the split opens without stealing focus from the agent.
- **Liveness / close:** `kitten @ close-window --match env:THIRDEYE_SESSION=<id>`
  — precise matching on our own marker, no title-guessing.
- **Target instance:** `KITTY_LISTEN_ON` is inherited by the hook environment
  and recorded at `SessionStart`, so the daemon knows which kitty to talk to.
  Its absence is exactly the "kitty not available" signal for
  `Surface::available()`.

---

## 13. Failure modes

| Failure | Behaviour |
|---|---|
| Daemon crashes | Next hook reactivates it; log replay restores state. Durable view state reloads from `viewstate.json`. |
| Socket connect fails | Event already durable in the log; latency-only loss. Hook retries once after starting the unit. |
| Head crashes | Window closes with the process. Daemon keeps running; policy decides reopen. |
| kitty not running | `Surface::available()` false → daemon logs a concrete message, keeps collecting events. Viewer simply never opens. |
| Session ends abruptly | Socket unit stopped by `SessionEnd`; if that never fires, daemon idle-exits. Orphan holds no unique data. |
| Torn log line | Counted in `ReadOutcome::malformed_lines`, not fatal. Already implemented and tested. |
| Oversized event | Unbounded fields truncated to stay under `MAX_LINE_BYTES`; event never dropped. Already implemented and tested. |

---

## 14. Testing strategy

- **Daemon is testable headless.** A `NullSurface` plus a scripted event log
  drives every policy transition and turn-grouping case without a terminal.
- **Protocol** gets round-trip tests over an in-memory transport, decoupled
  from unix sockets.
- **Surface** is the only genuinely untestable-in-CI part — a good argument for
  keeping it as thin as it is. One manual end-to-end smoke test covers kitty.
- **Perturbation testing** stays a project rule: any new indicator must be
  shown to go red on a planted defect before its green is trusted. Existing
  precedent: `staleness_detects_a_real_on_disk_change`.
- **Existing gates** continue to apply: `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`.

---

## 15. Deferred decisions

1. **Per-session vs global daemon.** PoC is per-session: it matches the
   one-head-one-session decision and keeps `TrackerState` untouched.
   Revisit if a cross-session view is ever wanted.
2. **Diff source for non-git cases.** Working-tree diff via git covers tracked
   files. Untracked files and non-git projects fall back to plain content for
   the PoC; a snapshot-based diff against what the agent first saw is the
   obvious later improvement, and the tracker already records that moment.
3. **Turn history depth.** Whether turns persist across a session restart, and
   whether old turns are pruned.
4. **Dynamic plugin loading.** Static registry for the PoC; extraction is
   mechanical because the capability seam already exists.

---

## 16. Implementation milestones

| # | Milestone | Status |
|---|---|---|
| 1 | `LogRecord` wrapper + `TurnMarker`; `EventKind::Flag`; runtime-dir paths | done |
| 2 | `thirdeye hook` binary + Claude Code adapter (append + notify) | done |
| 2b | `thirdeye ls` — headless stand-in for the head | done |
| 3 | MCP server + flag tool + the skill teaching Claude when to use it; `ls` renders flags | done |
| 4 | Daemon skeleton: socket activation, log tailing, turn grouping, `NullSurface` | done |
| 5 | Head protocol + in-memory transport tests | done |
| 6 | ratatui head: list, turn groups, detail view, `★` ordering | done |
| 7 | `Surface` trait + kitty implementation | done — verified live against kitty 0.48.0 |
| 8 | Policy engine + visibility rules + slash commands | done — verified live |
| 9 | Plugin packaging: manifest, hooks, units installer (rootless) | done — verified live |
| 10 | End-to-end smoke test in kitty | done — `scripts/e2e.sh`, passed |

**Why attention moved from 8 to 3.** The original order put the flag tool last
because the `★` treatment in §10.1 is a TUI feature, and a producer with no
renderer looks pointless. That reasoning conflated the two halves. The flag
tool is a *second producer* writing `mcp.jsonl`, and §7 already merges
producers by timestamp, so it depends on nothing above milestone 2 — while the
daemon, protocol, head and surface all sit between it and its original slot.

Deferring it also left the log's most distinctive claim untested. Every other
event kind is inferred from a tool call the agent was going to make anyway;
`Flag` is the only one the agent must *choose* to emit, which makes it the only
one whose real risk is a skill that never fires. That is a prompt-design
question, and it wants the shortest possible feedback loop — which `ls`
provides today and the TUI would not improve on.

`2b` is listed because it was built and is load-bearing (it is how milestone 2
was verified), not because the original table anticipated it. It is what makes
this reordering cheap: milestone 3 needs a renderer, and there already is one.
The TUI still owns `★` ordering and the reason line when milestone 6 lands; `ls`
only has to make flags *visible* enough to prove the mechanism works.
