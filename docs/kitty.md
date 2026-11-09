# Running thirdeye in kitty

The viewer opens in a kitty split beside your agent.

**Verified live on 2026-08-01** against kitty 0.48.0: hook records
`KITTY_LISTEN_ON` → daemon selects the kitty surface → `kitten @ launch`
opens a window running `thirdeye head` → the head connects and renders the
turn and its files → `close-window --match env:` tears it down. The launched
window came up **unfocused**, as `--keep-focus` promises.

**Re-verified on 2026-10-08** against kitty 0.48.0, after hiding gained
conditions: `m`, `q` and `Q` each held and lifted as specified, a window
closed by hand counted as `q` and came back on the next turn's edit,
`thirdeye open` reopened after a session-long hide, and `SessionEnd` took the
window with it. Driven by feeding `thirdeye hook` captured payload shapes
rather than by a live agent; `scripts/e2e.sh` covered the live agent the
same day.

This page is the procedure, and the troubleshooting for when your setup
differs from the one it was verified on.

## What has to be true first

| Requirement | How to confirm |
|---|---|
| kitty's remote control is on | `grep allow_remote_control ~/.config/kitty/kitty.conf` → `socket-only` (or `yes`) |
| kitty listens on a socket | `grep listen_on ~/.config/kitty/kitty.conf` → e.g. `unix:/tmp/kitty-{kitty_pid}` |
| `thirdeye` is on `PATH` | `which thirdeye` — for you; the split itself runs the head by the daemon's own full path |
| the plugin is installed in the project | `docs/install.md` steps 1–2 |
| the daemon units are installed | `docs/install.md` step 3 |

After editing `kitty.conf`, kitty must be **restarted** — it does not re-read
`listen_on` on config reload.

## The run

**1. Start kitty, and check it is listening.**

```bash
kitty &
```

Then *inside that kitty window*:

```bash
echo "$KITTY_LISTEN_ON"     # must print something like unix:/tmp/kitty-12345
```

An empty value is the whole ballgame: thirdeye treats its absence as "no
surface available" (§12) and will never try to open a split.

**2. Start the agent from inside that same kitty window.**

```bash
cd ~/path/to/a-test-project
claude
```

The hook inherits `KITTY_LISTEN_ON` and records it beside the session socket —
the daemon is started by systemd and has none of the agent's environment, so
this is the only way it can learn which kitty to drive.

**3. Ask the agent to change a file.**

Reads alone deliberately do not open the viewer (§10.2): reading is constant,
and a split that opened for it would always be open. A `Write` or `Edit` — or
an explicit flag — is what triggers it.

## What should happen

- A new kitty window appears in the agent's tab, titled `thirdeye` — **where
  is the tab's layout's decision**, not thirdeye's: below the agent in kitty's
  default `fat`, beside it in `tall` or `splits` (`enabled_layouts tall` in
  `kitty.conf`). kitty's placement and size options mean something different
  in every layout, so none is passed. For a pane that is always beside the
  agent at a size you choose, run the agent inside tmux or Zellij in kitty.
- **Focus stays in the agent.** `--keep-focus` is deliberate; a viewer that
  grabs the cursor mid-sentence is worse than no viewer.
- The split lists the turn, its files, and any flagged file first with `★` and
  the reason.
- As the agent keeps working, the list updates without you touching anything.

Inside the split: `j`/`k` or arrows to move, `Enter` to open a turn or file,
`Esc` to close the detail pane. A file opens at the lines the agent touched,
marked `▌`; `d` switches it to its `git diff` and back. Closing the split takes a condition — `m`
until the next edit, `q` until the next turn's first edit, `Q` for the
session; closing the window by hand counts as `q`. A new flag brings it back
regardless. `docs/tmux.md` has the full table.

## If it does not appear

Work down this list; each step rules out one layer.

```bash
# 1. Did the hook record the kitty address?
cat "${XDG_RUNTIME_DIR}/thirdeye/"*.json
#    Expect: {"session":"…","workspace":"…","env":{"KITTY_LISTEN_ON":"unix:/tmp/kitty-…"}}
#    No KITTY_LISTEN_ON -> the agent was not started inside kitty, or kitty has
#                          no listen_on. Go back to step 1 of The run.
#    TMUX present too   -> the agent is in tmux inside kitty, and gets a tmux
#                          pane instead. Set THIRDEYE_SURFACE=kitty to override.

# 2. Is the daemon running, and what did it decide?
systemctl --user list-units 'thirdeye@*'
journalctl --user -u 'thirdeye@*.service' -n 20 --no-pager
#    Expect a line like: opening the view for session … in kitty (N turns)
#    Nothing at all     -> the hook never reached the socket; check
#                          ${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye/hook-errors.log

# 3. Does kitty accept remote control at all?
kitten @ --to "$KITTY_LISTEN_ON" ls | head -5
#    A permission error -> allow_remote_control is not enabled.

# 4. Can it launch a window by hand?
kitten @ --to "$KITTY_LISTEN_ON" launch --type=window --keep-focus \
    --title=thirdeye-probe sh -c 'echo hello; sleep 5'
#    Fails here -> the problem is kitty's, not thirdeye's, and the exact error
#                  is the thing to report.

# 5. Is the events pipeline working at all, independent of kitty?
thirdeye ls
#    Empty -> the problem is upstream of the surface entirely; docs/install.md
#             has the capture-pipeline troubleshooting.
```

The head also runs standalone in **any** terminal, which separates "the viewer
is broken" from "the surface is broken":

```bash
thirdeye head --session <id>
```

## One environment trap

The split inherits **kitty's** environment, not the daemon's. In a normal
install both use the same defaults and agree, but if you run the daemon with a
non-default `$THIRDEYE_STATE_DIR` or `$XDG_RUNTIME_DIR` — as any test harness
does — the head launched into kitty will look for the socket somewhere else
and come up empty, showing `nothing recorded yet` beside a daemon that is
working perfectly.

This bit the author twice while verifying the page. If the split opens but
stays empty, that is the first thing to suspect.

## Cleaning up a stray split

The window is tagged with its session, so it can be closed precisely rather
than by title:

```bash
kitten @ --to "$KITTY_LISTEN_ON" close-window --match env:THIRDEYE_SESSION=<id>
```
