# Plan — daemon skeleton (spec milestone 4)

Implements §4.1, §6 and §10.2 of
`docs/design/specs/2026-08-01-thirdeye-poc-design.md`.

Goal: a per-session daemon that tails the log, groups turns, decides
visibility, and drives a `Surface` — with no terminal and no systemd required
to test any of it.

## Decisions taken before implementation

### D1 — `forbid(unsafe_code)` stays absolute; `listenfd` acquires the fd

Socket activation hands the daemon an inherited listening socket as fd 3.
Turning that into a `UnixListener` needs `OwnedFd::from_raw_fd`, which is
`unsafe`. Probed, not assumed:

- `OwnedFd → UnixListener` is a **safe** conversion and compiles under
  `#![forbid(unsafe_code)]`.
- Acquiring fd 3 is the only unsafe step, and `forbid` cannot be overridden by
  an inner `#[allow]` — that is the difference between `forbid` and `deny`.

**Decision: take `listenfd` (v1.0.2) in pass 2.** The workspace lint stays
`forbid` with zero exceptions in code we write.

Rejected:
- *A quarantine crate opting out of the lint.* Keeps the dependency count at
  zero but makes "unsafe is forbidden workspace-wide" true only with an
  asterisk, and the asterisk is a whole crate.
- *Downgrading the workspace lint to `deny`.* Buys one exception by weakening
  the invariant everywhere, including in crates that will never need it.

### D2 — activation is a seam, so the core does not depend on systemd

§5 already names `Activation` as one of the five seams. Making it a trait now
means the daemon core is testable without systemd, and the non-systemd
fallback of §6.3 is additive rather than a rewrite.

```rust
trait Activation { fn listener(&self) -> Result<UnixListener>; }
struct SelfBind { path: PathBuf }      // pass 1: binds its own socket
struct SocketActivated;                // pass 2: takes the inherited fd
```

`SelfBind` needs no unsafe and no systemd, so every test below runs anywhere.

### D3 — `Surface` arrives here, not at milestone 7

Milestone 4 lists `NullSurface`, but the `Surface` trait was scheduled for
milestone 7. A Null implementation of a trait that does not exist is not a
thing. **The trait lands here in minimal form** (`available`, `open`, `close`,
`is_open`); the kitty implementation stays at milestone 7. §14 already assumes
this — "a `NullSurface` plus a scripted event log drives every policy
transition ... without a terminal".

### D4 — tailing re-reads the session, it does not track byte offsets

The socket notification is advisory (§4.2): the daemon learns *that* something
happened from the socket and *what* from the log. On each notification the
daemon re-reads the session through `read_session`.

Re-reading is not merely simpler than incremental offsets — it is *more*
correct, because dedup and timestamp-merging across producers are defined over
the whole session, not over a suffix. Sessions are small (the live capture ran
to a few hundred lines). Incremental reading is a later optimization that must
preserve this behaviour, not replace it.

## Pass 1 — the core (no systemd, no terminal)

| # | Task | Done when |
|---|---|---|
| 1 | `Surface` trait + `NullSurface` recording every decision | trait has no terminal concepts in it |
| 2 | `Activation` trait + `SelfBind` | daemon binds and accepts on a temp path |
| 3 | `Hello { role, protocol_version }` handshake | hook and head roles distinguished on one socket; incompatible versions rejected |
| 4 | Daemon loop: notify → re-read → group → decide → drive surface | scripted log drives `NullSurface` through open/close |
| 5 | Visibility rules of §10.2 | mutation or flag opens; reads alone never do |

## Pass 2 — the systemd wiring

| # | Task | Status |
|---|---|---|
| 6 | `SocketActivated` via `listenfd`; `sd_notify` READY=1 (hand-rolled, safe) | done |
| 7 | Hook's notify half: connect, one advisory line, close; `ENOENT` → start unit, retry once | done |
| 8 | Record the session id and workspace where the daemon can find them | done — see D5 |
| 9 | Unit templates + manual install doc (the *installer* is milestone 9) | done |
| 10 | Live smoke test under a real session | done |
| 11 | `SessionEnd` → stop the unit; record `KITTY_LISTEN_ON` | **open** |

### D5 — the instance name is not enough, so the hook writes a pointer

Wiring the units exposed a gap the spec did not anticipate. `ExecStart=thirdeye
daemon --session %i` gives the daemon *only* the instance name, and that is
insufficient twice over:

- **`%i` is the socket stem, not the session id.** Equal for a UUID, but a
  session id too long for `sun_path` is hashed into the stem, and a hash does
  not invert.
- **Nothing names the workspace.** The daemon has systemd's working directory
  and none of the agent's environment, and the workspace slug is
  `basename + hash` — also not invertible. Left alone the daemon resolves some
  *other* workspace and reports nothing while every other part works, which is
  the worst kind of failure this project can ship.

So the hook writes `<runtime>/<stem>.json` beside `<runtime>/<stem>.sock`
holding the real session id and the absolute workspace root. Only the hook
environment has those facts, which is exactly why it is the writer — the same
reasoning §6.2 already applies to `KITTY_LISTEN_ON`. The daemon removes the
file when it exits, since `RemoveOnStop` knows only about the socket.

## Verified live (2026-08-01, systemd 255)

Hook appended → recorded the pointer → poked the socket → systemd activated
the service → the daemon inherited the fd through `listenfd` → recovered the
workspace from the pointer → replayed the log → opened the view. Journal order
(`Starting` … daemon line … `Started`) confirms `Type=notify` and the
hand-rolled `READY=1`. Stopping the socket unit stops the daemon.

## Known gaps

- **`%t/thirdeye` is `0755` under systemd.** systemd creates the directory when
  it owns the socket, and `RuntimeDirectory=` cannot be used to tighten it —
  the directory is shared between sessions, so tying its lifetime to one unit
  would delete another session's socket. The sockets themselves are `0600`, so
  what is exposed is the *list* of live session ids, not access to them.
  `SelfBind` creates the directory `0700` because it can.
- **`SessionEnd` does not stop the unit.** A finished session leaves an idle
  daemon until logout. Harmless — it holds no unique data — but untidy, and it
  is what task 11 is for.

## What the pass 1 tests do not cover

Stated because a green suite that hides its blind spots will eventually vouch
for something it never examined:

- **The inherited fd itself.** `SelfBind` exercises every line of the daemon
  except the one that takes fd 3 from systemd. That line is covered only by
  the live smoke test in pass 2.
- **Unit file correctness.** Nothing in-process reads them.
- **The hook→socket hop.** Pass 1 drives the daemon by connecting directly.

Per the project rule, each visibility indicator gets a perturbation test:
prove it goes red on a planted defect before trusting its green.
