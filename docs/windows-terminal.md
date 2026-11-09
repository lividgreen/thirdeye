# Running thirdeye in Windows Terminal, from WSL

The viewer opens in a **new Windows Terminal window**, not in a split beside
your agent — and only when the agent flags a file, or when you ask for it.
The reason is below. **For a split, run the agent inside tmux or Zellij** in
Windows Terminal (`docs/tmux.md`, `docs/zellij.md`): the viewer is then a
pane beside the agent that never takes focus, and it opens on every edit.

**Verified live on 2026-10-11** against Windows Terminal 1.24.12741.0 on
Windows 10 (19045), WSL 3.0.1.0, Ubuntu 26.04 with systemd, by a real Claude
Code session started from a WSL shell in a Windows Terminal pane: hook
records `WT_SESSION` and `WSL_DISTRO_NAME` → the socket-activated daemon
resolves `windows-terminal` → the agent's flag opens a new window running the
head → the agent exits, the daemon tells the head to close, and the window
goes with it — the window seen on screen, twice, showing the turn and the
flagged file. tmux 3.6 and Zellij 0.45.1 were verified in the same Windows
Terminal the same day.

## What has to be true first

| Requirement | How to confirm |
|---|---|
| the agent runs in WSL, in a Windows Terminal pane | `echo $WT_SESSION $WSL_DISTRO_NAME` prints both |
| `wt.exe` is on the agent's `PATH` | `command -v wt.exe` — it is by default; WSL appends the Windows `PATH` unless `appendWindowsPath=false` in `/etc/wsl.conf` |
| the binary, plugin and units agree | `cargo xtask install`, then `thirdeye doctor`, which says `would use windows-terminal` |

## How it is driven

- **Open:** `wt.exe -w new new-tab --title thirdeye wsl.exe -d <distro> --cd
  <workspace> -e <head> head --launched-by-surface --session <id>`.
  `<distro>` is the agent's `WSL_DISTRO_NAME`; the head is a Linux program
  and `wt.exe` starts Windows ones, so it is entered through `wsl.exe`.
- **Close:** nothing in the CLI closes a window or a pane. The daemon sends
  the head `Close`, the head exits normally, and Windows Terminal closes a
  window whose command exited normally — as for Ghostty, Alacritty and GNOME
  Terminal (`docs/windows.md`).
- **Policy:** a window appears over the agent and takes focus, so the
  session starts in `attention`. `thirdeye policy any` opens it on every
  edit.

## Why it is not a split

`wt.exe split-pane` exists, and was measured — with a probe in each new pane
logging its size and the focus reports (`CSI ? 1004 h`) Windows Terminal sent
it, and a human reading off where the pane landed:

- **It cannot be aimed.** It splits whichever pane has focus, and no option
  names another. With focus in another tab of the agent's window, the pane
  opened in that tab; with focus in a second Windows Terminal window, in
  that window; a second split fired while the first still had focus split
  the first (22 of its 60 columns).
- **`WT_SESSION` does not select anything.** `-w 0` is "the most recently
  used window" — Microsoft's own wording — and went to the focused window
  with the agent's `WT_SESSION` in the environment and without it. A request
  to address a tab by `WT_SESSION` is open upstream (microsoft/terminal
  #19783).
- **The new pane takes focus.** `move-focus previous` chained onto the split
  with `;` did nothing; run as a second call 1.5 s later it did hand focus
  back, and keys typed in between went to the viewer.

So a split here follows the human, not the agent: right while they watch the
agent, wrong whenever they are in another tab or window — which is when an
agent working unattended is most likely to edit something. Guake is not
planned for the same limitation.

What does work, and would be the basis of a split if `wt.exe` learns to aim
one: `split-pane -V -s 0.4` took 40% of the pane it split (60 of 152
columns), and a pane whose command exits normally closes itself.

## Facts that shaped it

- **A systemd user service can run `wt.exe`.** Interop needs nothing from the
  agent's environment — no `WSL_INTEROP`. But `wt.exe` is not on the user
  manager's `PATH`; it is found on the agent's, which the hook records.
- **`wt.exe` parses its arguments again.** A raw `;` in any of them ends one
  command and starts another, so a `;` in a session id or a workspace path
  is written `\;` (measured: the argument and the directory arrive whole).
  A `"` cannot be written at all — `q"q`, `back\slash` and `$HOME` arrived as
  one argument — so an id or path holding `"` or `\` opens no window rather
  than a wrong command.
- **Spaces and single quotes pass through** unchanged, in arguments and in
  `--cd`.
- **A window whose command fails stays open**, showing the error; only a
  normal exit closes it. The unescaped `;` above left a window of three
  tabs, one per piece of the command line, each trying to run its piece.
- **`WT_SESSION` and `WSL_DISTRO_NAME` are in every terminal started from a
  WSL shell** — a kitty or Ghostty under WSLg keeps them. So this is the last
  surface tried, and it also requires `TERM=xterm-256color`.
- `--size <columns>,<rows>` sets the new window's size (measured: `90,30`
  gave 90 columns; the default was 120). Not used yet.

## Known gaps

- **The window inherits Windows Terminal's environment, not the daemon's.**
  A non-default `$THIRDEYE_STATE_DIR` set only in the agent's shell is not
  seen by the head, which then shows nothing — the same trap as
  `docs/kitty.md`.
- A workspace on the Windows drive is case-insensitive, and the workspace
  is identified by the text of its path: `/mnt/c/Users/me/app` and
  `/mnt/c/USERS/me/app` are one directory and two workspaces. Start the
  agent and run `thirdeye ls` from the same spelling.
- An agent on Windows itself, not in WSL, is another page:
  `docs/native-windows.md`.
