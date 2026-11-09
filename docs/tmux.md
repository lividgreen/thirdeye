# Running thirdeye in tmux

The viewer opens in a tmux pane beside your agent. tmux works inside any
terminal emulator, and it is the only surface that survives SSH: a local
emulator's control socket is unreachable from the host the agent runs on,
while the tmux server is right there.

**Verified live on 2026-10-08** against tmux 3.4: hook records `TMUX` and
`TMUX_PANE` → the socket-activated daemon resolves the tmux surface →
`split-window -d` opens a pane beside the agent's own, running `thirdeye
head` → the head connects and renders the turn and its files → each way of
hiding and reopening behaves as below → `SessionEnd` takes the pane, the
daemon and the session pointer with it. The new pane came up **unfocused**,
with a bystander pane present to catch a close aimed at the wrong one.

**Codex too, on 2026-10-08** (codex-cli 0.161.0): `codex exec` run inside a
tmux pane in a project wired by `thirdeye install -a codex` — its shell edit was
observed, the daemon logged `opening the view … in tmux`, the head's pane
opened beside it unfocused, and was gone once the session ended. Codex's
hooks inherit the agent's environment, so nothing surface-side is
Codex-specific.

**OpenCode too, on 2026-10-08** (2.0.24, `opencode run --standalone`): its
edit opened the head's pane beside it unfocused, gone when the server shut
down. Standalone only — see the background-service gap in `docs/install.md`.

**pi too, on 2026-10-08** (1.1.0, `pi -p`): its edit opened the head's pane
beside it unfocused, gone when pi exited.

The run drove `thirdeye hook` and `thirdeye mcp` from inside a tmux pane with
the payload shapes in `docs/hook-payload.md`, rather than a live agent. That
exercises everything from the hook down; it does not exercise Claude Code
itself, which `scripts/e2e.sh` does.

## What has to be true first

| Requirement | How to confirm |
|---|---|
| tmux is installed | `tmux -V` — `split-window -l 40%` needs 3.1 or later |
| nothing about `PATH` | the pane runs the head by the daemon's own full path — tmux resolves a pane's command with the `PATH` of the client asking for the split, which is the daemon's, and under systemd that may not hold `~/.local/bin` (it does not on WSL) |
| the plugin, binary and units agree | `cargo xtask install`, then `thirdeye doctor` |

## The run

Start tmux, and the agent inside it:

    tmux new -s work
    claude

`echo $TMUX $TMUX_PANE` inside that pane must print a socket path and a pane
id, and `thirdeye doctor` must say `would use tmux`. Inside tmux *inside
kitty* it lists both and still chooses tmux: the innermost container wins.
`THIRDEYE_SURFACE=kitty` (or `none`) overrides that.

## What should happen

1. **An edit opens a pane beside the agent.** A read alone never does (§10.2).
2. **Focus stays in the agent pane.** Keep typing as the pane opens.
3. **The pane runs the head**, at the workspace root, showing the turn.
4. **Hiding takes a condition.** In the head:

   | Key | Hidden until |
   |---|---|
   | `m` | the agent's next edit, even in this turn |
   | `q` | the first edit after the next prompt |
   | `Q` | the end of the session |
   | ctrl-c, or killing the pane by hand | same as `q` — it cannot say otherwise |

   `/thirdeye-close next-mutation | next-turn | session` does the same from
   the agent. A new flag brings the viewer back whichever was chosen; a new
   prompt alone never does. `/thirdeye-open` always does.
5. **Closing removes that pane and no other.**
6. **`SessionEnd` takes the pane with it**, because the head exits when its
   daemon does.

## If it does not appear

```bash
cat "${XDG_RUNTIME_DIR}/thirdeye/"*.json
#    Expect "env":{"TMUX":"/tmp/tmux-…,…","TMUX_PANE":"%…"}
#    Missing -> the agent was not started inside tmux.

journalctl --user -u 'thirdeye@*.service' -n 20 --no-pager
#    Expect: opening the view for session … in tmux (N turns)
#    "in kitty" -> THIRDEYE_SURFACE=kitty is set, or TMUX_PANE is missing.

cat ~/.local/state/thirdeye/*/<session>/hide.json
#    Present -> the viewer is hidden, and says until when.
```

## Known gaps

- A pane closed by hand is noticed when its head disconnects, which is
  immediate — but a head that never connected (say the binary was removed
  while its daemon ran) leaves the daemon believing in a pane that showed an
  error and closed. `thirdeye close` then `thirdeye open` recovers it.
- The split is `-h -l 40%` unconditionally. A narrow terminal gets a narrow
  head.
