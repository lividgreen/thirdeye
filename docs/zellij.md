# Running thirdeye in Zellij

The viewer opens in a Zellij pane beside your agent. Like tmux, Zellij works
inside any terminal emulator and survives SSH.

**Verified live on 2026-10-09** against Zellij 0.45.1, inside kitty with a
client attached: hook records `ZELLIJ_SESSION_NAME` and `ZELLIJ_PANE_ID` →
the socket-activated daemon resolves the Zellij surface → `new-pane
--direction right --no-focus` opens a pane beside the agent's own, running
`thirdeye head` → the head renders the turn and its files → `SessionEnd`
closes the pane. The pane came up **unfocused** with a second, focused pane
present, so it was the agent's pane that was split, not the focused one.
Like the tmux check, the run drove `thirdeye hook` with captured payload
shapes rather than a live agent.

## What has to be true first

| Requirement | How to confirm |
|---|---|
| Zellij 0.45 or later | `zellij --version` — `new-pane` must print the new pane's id, and `close-pane` and `resize` must take `--pane-id` |
| nothing about `PATH` | the pane runs the head by the daemon's own full path, so the Zellij server's `PATH` need not hold it |

## How it is driven

The daemon runs outside Zellij, so every call names the session: `zellij
--session <ZELLIJ_SESSION_NAME> action …`.

- **Open:** `env ZELLIJ_PANE_ID=<agent's pane> zellij --session <s> action
  new-pane --direction right --no-focus --close-on-exit --cwd <workspace>
  --name thirdeye -- thirdeye head --launched-by-surface --session <id>`. It
  prints `terminal_<n>`, the pane's only reliable handle.
- **Size:** Zellij splits a pane in half. The pane is then moved toward
  `THIRDEYE_SPLIT` (default 40%) with `resize --pane-id terminal_<n>
  decrease left`, one 5% step at a time (measured: 103 → 93 → 82 → 72 of 206
  columns). Focus is untouched.
- **Close:** `close-pane --pane-id terminal_<n>`.

## Facts that shaped it

- **`--no-focus` places the pane "relative to the pane the command was
  issued from", and from outside Zellij that is read from
  `ZELLIJ_PANE_ID`.** Without it the pane split whichever pane had focus,
  which need not be the agent's (checked by leaving it out).
- **`zellij` is looked for on the agent's `PATH`, not the daemon's.** The
  hook records it and the daemon searches and runs with it. Found on WSL
  (2026-10-11, a live Claude Code session): a Zellij from `cargo install`
  sits in `~/.cargo/bin`, which a systemd user manager's `PATH` does not
  hold, and the session ran with no surface at all.
- **A command pane outlives its command** and waits for a keypress unless
  opened with `--close-on-exit`. Without it, a head that exits because its
  daemon went away would leave a dead pane behind.
- The command after `--` is an argv, not a shell string, so no session id
  can smuggle in a second command.
- The first start of Zellij shows a floating "About Zellij" pane that holds
  focus; a pane opened while it was focused did not appear in the layout.
  Close it once.
- **Nesting.** Zellij inside tmux, or tmux inside Zellij, leaves both
  sets of variables, and nothing in the environment says which is inner.
  tmux is tried first. Set `THIRDEYE_SURFACE=zellij` to choose.

## On Windows

The same calls, verified natively on 2026-10-15 — see
`docs/native-windows.md`.
