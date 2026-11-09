# Running thirdeye in WezTerm

The viewer opens in a WezTerm split beside your agent.

**Verified live on 2026-10-08** against wezterm 20260929-043349-cab25161, by
driving `thirdeye hook` from inside a pane of a private WezTerm instance with
captured payload shapes: an edit opened a split to the right of the agent's
pane, focus stayed on the agent, the head rendered the file, `q` hid it until
the next turn's edit, a split closed by hand came back on the next turn's
edit, and `SessionEnd` took it away.

## What has to be true first

| Requirement | How to confirm |
|---|---|
| the agent runs inside WezTerm | `echo $WEZTERM_UNIX_SOCKET $WEZTERM_PANE` prints a socket path and a pane id |
| nothing about `PATH` | the split runs the head by the daemon's own full path, so WezTerm's `PATH` need not hold it |
| binary, plugin and units agree | `cargo xtask install`, then `thirdeye doctor` — it must say `would use wezterm` |

Inside tmux inside WezTerm you get a tmux pane instead: the innermost
container wins. `THIRDEYE_SURFACE=wezterm` overrides that.

## How it works, and why

- **The instance is named by its socket.** The daemon is socket-activated and
  has none of your terminal's environment, so the hook records
  `$WEZTERM_UNIX_SOCKET` and every `wezterm cli` call carries it.
- **Every call passes `--no-auto-start`.** Without it, a call that finds no
  GUI — WezTerm closed, say — spawns a `wezterm-mux-server` in the background.
- **`split-pane` takes focus**, and has no flag to prevent it. thirdeye hands
  focus back at once to whichever pane was active in the agent's tab. If you
  are looking at another tab, you stay there.
- **kitty started from a WezTerm shell is still kitty.** kitty keeps the
  `WEZTERM_*` variables it inherits, so detection also checks `TERM`, which
  kitty sets to `xterm-kitty` for its own children.
- **The head is argv, not a shell string**, so a session id reaches no shell.
- **Closing kills the pane by the id `split-pane` printed** — never by title.

## If it does not appear

```bash
cat "${XDG_RUNTIME_DIR}/thirdeye/"*.json
#    Expect "env":{"WEZTERM_PANE":"…","WEZTERM_UNIX_SOCKET":"…/gui-sock-…"}
journalctl --user -u 'thirdeye@*.service' -n 20 --no-pager
#    Expect: opening the view for session … in wezterm (N turns)
```

## Known gaps

- When the head closes its own split (`q`), the daemon's follow-up
  `kill-pane` finds it gone, and WezTerm leaves a small log file in
  `$XDG_RUNTIME_DIR/wezterm/`. Harmless; cleared at logout.
- The split is `--right --percent 40` unconditionally.

## On Windows

The same calls, verified natively on 2026-10-15 — see
`docs/native-windows.md`.
