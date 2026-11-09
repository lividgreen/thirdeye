# Terminals that cannot split: a window instead

Ghostty, Alacritty and GNOME Terminal cannot put the viewer in a split
beside the agent: Ghostty and GNOME Terminal expose no call that splits a
given pane, and Alacritty has no splits at all. Each can open a **new
window** running the viewer, so that is what thirdeye does there.
Windows Terminal is a fourth, for a different reason — it splits, but only
the pane that has focus — and has its own page, `docs/windows-terminal.md`.

**For a split in any of these, run the agent inside tmux or Zellij** in that
terminal (`docs/tmux.md`, `docs/zellij.md`): the viewer is then a pane beside
the agent that never takes focus, and it opens on every edit rather than
only on a flag.

**Verified live on 2026-10-09** through the real hook and daemon, against
Ghostty 1.3.1, Alacritty 0.13.2 and GNOME Terminal 3.52.0:

| Step | Every one of the three |
|---|---|
| an edit, nothing flagged | no window |
| `thirdeye open` | a window running the viewer |
| `thirdeye close` | the viewer exits, and its window with it |
| a flag | the window opens |
| `SessionEnd` | the viewer exits, and its window with it |

## How each is driven

| Terminal | Detected by | Opens with |
|---|---|---|
| Ghostty | `TERM=xterm-ghostty` | `ghostty +new-window --working-directory=<ws> --title=thirdeye -e thirdeye head …` |
| Alacritty | `ALACRITTY_SOCKET`, and `TERM` `alacritty` or `xterm-256color` | `alacritty msg --socket <socket> create-window --working-directory <ws> -T thirdeye -e thirdeye head …` |
| GNOME Terminal | `GNOME_TERMINAL_SERVICE` and `GNOME_TERMINAL_SCREEN`, and `TERM=xterm-256color` | `env GNOME_TERMINAL_SERVICE=<service> gnome-terminal --window --title=thirdeye --working-directory=<ws> -- thirdeye head …` |

- `ghostty +new-window` asks the running instance for a window over D-Bus;
  no second Ghostty process starts.
- Alacritty's socket and GNOME Terminal's service are recorded by the hook,
  so the window opens in the instance the agent runs in.
- All three ran from an environment like the daemon's under systemd: only
  `HOME`, `PATH`, the session bus and the displays, which the user manager
  has.

## What is different from a split

- **Nothing names the window afterwards**, so it is closed by ending the
  viewer in it: the daemon sends its head `Close`, the head exits cleanly
  (never by signal, which some terminals mark with a banner), and the
  terminal closes the window. A window that finishes opening after the view
  was closed is told to go as soon as its head connects.
- **It opens unasked only when the agent flags a file.** A window appears
  over the agent rather than beside it, and takes focus; a split does
  neither. So these sessions start in the `attention` policy. Your own
  `/thirdeye-policy` (or `thirdeye policy <mode>`) overrides it.
- **Focus moves to the new window.** None of the three can open one without
  focusing it.
- **GNOME Terminal closes the window only if the profile says so**:
  Preferences → profile → Command → "When command exits" must be "Exit the
  terminal", the default. `thirdeye doctor` warns when it is not.

## Facts that shaped it

- **Alacritty passes on `TERM_PROGRAM`** from the shell that started it:
  started from Ghostty, it still says `TERM_PROGRAM=ghostty`. So Ghostty is
  detected by `TERM`, which Ghostty sets fresh.
- **Every terminal's variables leak into terminals started from its
  shells**, so each surface also checks `TERM`. GNOME Terminal's windows come
  from its server, never from the launching shell, so its variables are the
  ones found in other terminals, never the reverse; it is tried last.
- These come after every splitting surface: in tmux or Zellij inside
  Ghostty, the viewer is a split.
- **Ghostty** gets a split when its scripting API ships (1.4), and
  **Konsole** and **xfce4-terminal** a tab or window when their packages
  are installed here (roadmap phases 2 and 3).
