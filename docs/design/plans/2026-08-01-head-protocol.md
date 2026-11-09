# Plan — head protocol (spec milestone 5)

Implements §11 and the model/presentation boundary of §4.3.

## Decisions

### D1 — the transport is a seam, so the protocol is testable without sockets

§14 asks for "round-trip tests over an in-memory transport, decoupled from
unix sockets". A protocol whose tests need a socket is a protocol whose
failures are timeouts, so `Transport` is a trait with two implementations:
`IoTransport` over a `UnixStream`, and an in-process pair used by tests. The
memory one is not a mock — it implements the same trait, so a protocol bug
surfaces there rather than hiding behind a socket that happens to work.

### D2 — one grammar, not two

`Hello` used to be an untagged first line with `ServerMessage` tagged after it.
That is two grammars for one connection. `ClientMessage` is now a single tagged
enum with `Hello` as a variant, so there is one decoder and one place to add a
message. Nothing had shipped, and `PROTOCOL_VERSION` covers the mismatch a
stale peer would see.

### D3 — nothing presentational crosses the wire

The daemon sends *semantic* data (§4.3). No colour, glyphs, styled text or
layout. The `★` beside a flagged file is a choice `thirdeye ls` makes and a web
head might make differently; what crosses is *that the file is flagged and
why*. Paths travel absolute with the workspace root sent once in the
`Snapshot`, because relativizing for display is presentation. There is a test
asserting no `★` can appear in an encoded `TurnView`.

Scroll offset deliberately stays out of `ViewState`: it depends on window size,
and pushing it into the daemon would reduce heads to dumb framebuffers and
force terminal semantics onto a future web head. Selection and folding *are*
in, because they are decisions the human made about the session.

### D4 — one thread per connection, and the lock is per message

A head holds its connection open for the whole session. Serving connections one
at a time — or holding the daemon lock for a conversation — would mean no hook
notification is processed while anyone is watching, which is exactly when the
view needs to update. So: a thread per connection, and `lock()` taken per
message. A poisoned lock is recovered rather than propagated, because the
daemon owns no unique data: the worst a panicked thread leaves behind is stale
derived state that the next `refresh` recomputes.

### D5 — `RequestDetail` is restricted to files the session touched

The requested path is client-controlled, and a head is not trusted more than
the socket it arrived on. Without a check, `RequestDetail` is an
arbitrary-file-read oracle for anything that can connect. Only paths the log
mentions can be read.

Perturbation-tested, per the project rule: with the check stubbed to `true`,
the daemon returned the contents of `/etc/passwd` over the socket, and the
guarding test went red. Its green is worth something.

### D6 — every message to a head goes through one channel

`refresh` now pushes a `Patch` to each attached head, which means two threads
have reason to write to one connection. On a newline-delimited protocol that is
not a slow message but a *torn* one: two writes interleaving produce a line
neither side can parse, and the connection is desynchronized from then on.

So each connection gets a channel and a single writer thread. Conversation
replies and pushed patches both travel through it, and the transport is written
by exactly one thread.

The daemon holds a clone of each head's sender, which introduces the matching
hazard: the channel cannot close while that clone lives, so a departed head
would leave its writer thread waiting forever. Heads are therefore attached
with an id and explicitly detached when their conversation ends — inferring it
from a failed send cannot work, because the send cannot fail.

## What is built

- `Transport` trait, `IoTransport`, in-memory pair.
- `ClientMessage`: `Hello`, `Select`, `ExpandTurn`, `RequestDetail`.
- `ServerMessage`: `Snapshot`, `Patch`, `Detail`, `Close`.
- `TurnView` / `FileView` / `FlagView` / `ViewState` — one row per file, kinds
  merged, newest range and newest flag reason win.
- A head gets a `Snapshot` on connect and a pushed `Patch` on every `refresh`,
  so it tracks the log rather than the moment it arrived.
- `ViewState` written through to `viewstate.json`, so selection and folding
  survive a head restart — verified by a test that makes a selection, throws
  the daemon away, and finds it again in the next `Snapshot`.
- View changes are *broadcast* rather than answered, so a second head cannot
  sit on a stale selection.
- `RequestDetail` answered with raw lines, for known files only.

Verified live over a real socket, not only in memory: a head connected, took
its snapshot, and received a `patch` naming the new file the instant a hook
appended it.

## What is not built yet

- **`DetailMode::Diff` returns content.** The reply says `Content` rather than
  claiming a diff a head would try to render. Real diffs need the git decision
  parked in §15.2.
- **`Detail` never sets `highlight`.** The range is in the model already; the
  field is wired but not populated.
