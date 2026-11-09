# The end-to-end smoke test

```bash
scripts/e2e.sh              # full chain, including kitty
scripts/e2e.sh --no-kitty   # everything except the surface
```

Spec milestone 10. Everything else in this repo is unit-tested; this is the
part that can only be checked by running it — a real Claude Code session, real
hooks, a real socket-activated daemon, a real kitty split.

**It costs money.** The agent step runs `claude -p`, which since 2026-06-15
draws on the Agent SDK credit pool rather than an interactive subscription. One
run is one small session.

## What it does

1. **Refuses to start if `thirdeye doctor --user` reports drift.** Without that a
   failure could mean "the code is wrong" or "something was stale", and those
   need different responses. This is the precondition that makes the rest of
   the output mean something.
2. Creates a scratch project in `/tmp`, writes two files into it, and wires the
   plugin in at `local` scope.
3. Starts a kitty with `--listen-on`, and exports `KITTY_LISTEN_ON` so the
   hook records it the way a real session would.
4. Runs one `claude -p` with a prompt that should cause a read, a mutation, and
   a recommendation.
5. Asserts on what landed, then removes everything it created.

## What it asserts

| Check | Why it is here |
|---|---|
| session directory exists | the workspace slug resolved the same for hook and reader |
| turn opened and closed | `UserPromptSubmit` and `Stop` both reached the log |
| a read and a mutation recorded | the adapter translated real payloads |
| **no file content in the log** | `tool_response` embeds whole files; the log must not become a copy of the source tree |
| **longest line ≤ 4096 B** | the atomic-append cap that concurrent `async` hooks depend on |
| daemon opened the view in kitty | socket activation, the visibility rule and surface resolution all worked |
| flag joined its turn | the attention path, end to end |
| no split outlived the session | `SessionEnd` took the window with it — the head exits when its daemon does |
| `ls` renders the files | the whole pipeline, read back |

## What it cannot assert

Stated because a test that hid its blind spots would be worse than no test.

- **Whether the agent chooses to flag.** That is the model's judgement — the
  thing the skill exists to influence and the thing no assertion can guarantee.
  It is reported as a *soft* note, not a failure. A run with no flag still
  proves the rest of the chain; it just says nothing about attention.
- **Whether the daemon is still running at check time.** `SessionEnd` stops it
  on purpose, so "not active" is usually correct behaviour rather than a fault.
  Also soft.
- **What the split looks like, or that it appeared at all.** By the time the
  checks run the session has ended and the window is gone, so the test sees
  only the daemon's word that it opened one, and that none remains. Nobody has
  looked at the pixels; `docs/kitty.md` is the procedure for that.

## Why it uses your real directories

It would be tidier to point `THIRDEYE_STATE_DIR` and `XDG_RUNTIME_DIR` at a
sandbox. It would also be wrong: the systemd units run under the *user
manager's* runtime directory, and the daemon it starts inherits none of the
test's environment. Overriding either makes the hook and the daemon disagree
about where the socket and the log live — the failure documented in
`docs/kitty.md`, which would then be caused by the test rather than caught by
it.

So the test uses the real locations, a scratch project, and a fresh UUID
session, and removes its own state, units and workspace directory on exit —
including on failure.

## A run that passed

```
== preconditions
  ok    thirdeye: thirdeye 0.1.0 (d465beee-dirty)
  ok    doctor: binary, plugin and units agree
== the log
  ok    turn opened by UserPromptSubmit
  ok    turn closed by Stop
  ok    a read was recorded
  ok    a mutation was recorded
  ok    no file content in the log
  ok    longest line 305B, within the 4096B cap
== the daemon
  soft  daemon not active now (it may have been stopped by SessionEnd)
  ok    daemon opened the view in kitty
== attention
  ok    mcp.jsonl written — the skill fired and the tool was called
  ok    a flag event was recorded
  ok    the flag joined its turn
== the viewer
  ok    no split outlived the session
  ok    ls renders the touched files
        turn 1 — Read data.txt. Then append a line saying 'checked' to notes.…
        ★ notes.md                                           edit,flag,read
              I appended 'checked' here — confirm the placement is what … (lines 3-4)
          data.txt                                           read
== verdict
  PASS — 1 soft note(s)
```
