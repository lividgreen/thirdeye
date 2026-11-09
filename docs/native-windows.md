# Running thirdeye on Windows itself

Not WSL: an agent started from PowerShell or Git Bash, and a `thirdeye.exe`.
Where the viewer opens depends on what the agent runs in, innermost first,
as everywhere:

| Agent runs in | The viewer opens as |
|---|---|
| Zellij 0.45.1 (native, since 0.44) | a pane beside the agent's, unfocused |
| WezTerm 20240203 | a pane beside the agent's; focus is handed back |
| Windows Terminal 1.24 | a new window, for the reason `docs/windows-terminal.md` gives — so only when the agent flags a file, or when you ask |

tmux has no Windows build; kitty, Ghostty and GNOME Terminal have no
Windows release; Alacritty runs on Windows but its `alacritty msg` IPC is
Unix-only, so thirdeye cannot ask it for a window.

**Verified live on 2026-10-12**, on Windows 10 (19045) with Windows Terminal
1.24 and Rust 1.99 (`x86_64-pc-windows-msvc`), by two real Claude Code
sessions (`claude -p`, started from PowerShell in a Windows Terminal pane):
the plugin's hooks recorded the prompt and a read → the hook started the
daemon → the agent's `flag`, through the plugin's MCP server, was recorded
and opened a window running the head → after the agent exited, the second
session's daemon, head and window were gone within half a second and nothing
was left in the runtime directory.

What that does **not** cover: the window was observed as a process, not by a
human looking at it; an interactive session; any agent but Claude Code;
Windows 11.

**Zellij and WezTerm verified on 2026-10-15**, as on Linux, by driving
`thirdeye hook` with the variables each records from a pane — not with a
live agent: a stand-in agent ran in a pane of each, the hook opened the
daemon, the daemon split that pane and ran `thirdeye head` there, the
head's screen (`zellij action dump-screen`, `wezterm cli get-text`) showed
the turn and its file, and `SessionEnd` closed the pane and stopped the
daemon. See "Zellij and WezTerm" below.

## What has to be true first

| Requirement | How to confirm |
|---|---|
| Git for Windows is installed | Claude Code runs the plugin's hooks through its Git Bash |
| the agent runs in a Windows Terminal pane | `echo $env:WT_SESSION` prints an id |
| the binary and plugin agree | `cargo xtask install`, then `thirdeye doctor`, which says `would use windows-terminal` |

Nothing else is installed: there are no units, because there is no service
manager to give them to.

## What is different from Linux

- **The hook starts the daemon, and the end of the session stops it.** No
  socket unit exists to start one on a connection, so a hook that finds no
  daemon starts `thirdeye daemon --session <stem> --detach`. Every hook of a
  turn that found none does; an exclusive lock on `<stem>.lock` lets one of
  them stay (`SelfBind::claim`). `SessionEnd` sends the daemon `End` over
  the socket, where Linux stops a unit.
- **The daemon is started in two steps.** A process started through Rust's
  std inherits every inheritable handle of its parent, the hook's own pipes
  among them. Measured: with the daemon holding them, `out=$(thirdeye hook
  < payload)` never returned. So the hook creates `daemon --detach` with
  nothing inherited (`winsafe::CreateProcess`, since std cannot ask for
  that), and that process — which has no pipe to hand on — starts the
  daemon with its stderr in `<stem>.log`.
- **A session that never says it ended is ended by watching the agent's
  process**, as on Linux, through the system's process list instead of
  `/proc`. It matters more here: the first real session left its daemon and
  window behind — neither `Stop` nor `SessionEnd` took effect when `claude
  -p` exited — and that was before the watch existed on Windows. Measured
  with a stand-in agent that exits without a word: the daemon was gone
  within its five-second poll.
- **Sockets are Unix sockets**, which Windows has had since 1803, through
  the `uds_windows` crate: std's own are unstable there.
- **State is under `%LOCALAPPDATA%\thirdeye`, sockets in its `run`
  directory**, and never under `HOME` or the temporary directory. Git Bash
  gives a hook a `HOME` and its own idea of `TEMP`; a viewer started by the
  terminal has neither, and the two must name one place.
- **The workspace is spelled without `\\?\`.** `canonicalize` returns
  `\\?\C:\app`, which no agent writes: the paths in its payloads would lie
  outside a workspace they are in, and `thirdeye hook` and `thirdeye ls`
  would name two workspaces.
- **The window inherits the daemon's environment**, unlike a window opened
  from WSL (measured: a variable set only in the launcher arrived in the
  new tab). A `THIRDEYE_STATE_DIR` set in the agent's shell reaches the
  head.

## How `wt.exe` is driven from Windows

`wt.exe -w new new-tab --title thirdeye -d <workspace> <thirdeye.exe> head
--launched-by-surface --session <id>`, and it closes as every such window
does: the daemon sends the head `Close`.

Measured through `std::process::Command`, each case in a window of its own
running a program that wrote down what it was given:

| Argument | Arrived as |
|---|---|
| `C:\…\with space` as `-d` | that directory |
| `C:\` as `-d` | `C:\` |
| `two words`, `it's` | unchanged |
| `back\slash`, `end\` | unchanged — unlike from WSL, where a `\` is refused |
| `a;b` | `a`, and the rest became a second command |
| `a\;b` | `a;b` |
| `q"q`, `tail` | one argument, `qq tail` |
| `%USERPROFILE%` | `C:\Users\<name>` |
| `$HOME` | unchanged |

So a `;` is written `\;`; and an id or a path holding a `"`, a pair of `%`,
or a `\` directly before a `;` opens no window rather than a wrong command.

## Zellij and WezTerm

Both are driven exactly as on Linux (`docs/zellij.md`, `docs/wezterm.md`);
everything those pages measured held here:

- **A pane's environment is the same.** Zellij sets `ZELLIJ_SESSION_NAME`
  and `ZELLIJ_PANE_ID`; WezTerm sets `WEZTERM_PANE` and a real Unix socket
  in `WEZTERM_UNIX_SOCKET` (`C:\Users\<name>\.local/share/wezterm\gui-sock-<pid>`).
  Both also keep a `WT_SESSION` from the terminal they were started in,
  which is why Windows Terminal is tried last.
- **Zellij:** `new-pane` printed `terminal_<n>`; with the agent's pane id
  in `ZELLIJ_PANE_ID` the agent's pane was split although another had
  focus, and without it the focused one was (checked both ways);
  `resize` moved about 5% of the window per step; `close-pane` closed it.
- **WezTerm:** `split-pane --percent 40` took 32 of 80 columns and printed
  the pane id; `activate-pane` handed focus back; `kill-pane` closed it.
  With its GUI gone, `cli --no-auto-start` failed and started no
  `wezterm-mux-server`.
- In both, the head's argv arrived whole, `;` included, and `--cwd` was
  honoured.

What it took on Windows:

- **No `env` program.** Both surfaces set a variable for one call by
  running `env K=V program …`; Windows has one only where Git's `usr\bin`
  happens to be on `PATH`. `Runner::run_with_env` now sets it directly; a
  test double still records the `env` form.
- **A program is found as `.exe`.** The runner looked for a file named
  `zellij`, found none, and the daemon would have used no surface beside a
  Zellij that was installed.
- **The head shortened paths as text**, trimming `/`: the file drew as
  `\a.txt`. It now strips the workspace by component — which also fixes
  `/w/abc/x` under `/w/a` drawing as `bc/x` on Linux.
- On first start Zellij shows a floating "Release Notes" pane that holds
  focus, as on Linux. Close it once.

## Facts that shaped it

- **`bash` on a Windows `PATH` is as likely WSL's as Git's.** The plugin's
  MCP server was `scripts/thirdeye-mcp.sh`; Claude Code started from Git
  Bash connected to it, and started from PowerShell — where `bash` resolved
  to `C:\Windows\System32\bash.exe` — reported `Connection closed`. It is
  now `scripts/thirdeye-mcp`, with a `thirdeye-mcp.cmd` beside it that
  Windows picks, and connects from both.
- **A checkout with `core.autocrlf` breaks the hook scripts**, which bash
  reads a carriage return into, and changes the embedded units and plugins.
  `.gitattributes` now keeps every text file LF, and `*.cmd` CRLF.
- **A connection closed with something unread is reset**, and the reset can
  overtake what was sent: `thirdeye policy` closed with the daemon's
  snapshot unread, and the daemon logged `os error 10054`. `open`, `close`
  and `policy` now say they are done and wait for the daemon to hang up.
- **The daemon has a console, and no window.** `CREATE_NO_WINDOW`: with no
  console at all, each `git` or `wt.exe` it ran would open one.
- A hook that starts nothing takes 50–75 ms; the first run of a newly built
  binary took about 900 ms.

## Known gaps

- **A file moved into the workspace by a shell command is not seen.**
  Windows keeps both of a moved file's times, and std reads no change time
  there; a copy is seen by its creation time.
- **The last turn of a `claude -p` session is often left open in the
  log.** Its `Stop` hook is async and the process exits around it: of four
  sessions on 2026-11-06, one recorded its final `Stop`, and one that idled
  on a background command recorded the `Stop` before it. It is not known
  whether an interactive session differs, but those in Zellij and WezTerm
  (below) both recorded their last `Stop`.
- **A Windows Terminal variable in another terminal** — an Alacritty
  started from a Windows Terminal shell — is taken for Windows Terminal.
  Zellij and WezTerm are tried first, so they are not affected.
- `thirdeye units` does nothing useful, and the other agents' installers
  write hook commands for `sh`. Only Claude Code has been run; installing
  another agent here says so.
- A daemon that is killed leaves its `.sock`, `.lock` and `.log` in
  `%LOCALAPPDATA%\thirdeye\run`; the next daemon for that session reuses
  them, and nothing removes those of a session that never returns.
- A path that differs from the root only in non-ASCII case is not
  respelled (`Workspace::respell` folds ASCII alone), and nor is a file
  name the agent spells in another case than the disk does: only the root
  is known to be in the disk's case.
- The hook script logs its errors under Git Bash's `HOME`
  (`~/.local/state/thirdeye/hook-errors.log`), not beside the state in
  `%LOCALAPPDATA%\thirdeye` — the one thing found through `HOME` here.

## The release, checked 2026-11-06

On Windows 10 19045, Windows Terminal 1.24, Claude Code 2.1.319 (`claude
-p` on Haiku, started from PowerShell) and Rust 1.99, against two pre-release
builds, published then as v0.0.2 and v0.0.1 and since folded into one
v0.0.1, with their archives served from a local directory
(`scripts/serve-release.ps1`; this machine has no Python), from a clean
machine: no earlier binary, plugin or state.

- **The gates pass on Windows**, after one test stopped assuming an `sh`
  on `PATH` — CI's runner has Git's `usr\bin` there, a PowerShell beside a
  stock Git for Windows does not.
- **`install.ps1`**: the binary lands in `%LOCALAPPDATA%\Programs\thirdeye`;
  the user `PATH` gains that entry and keeps the rest exactly, unexpanded
  `%USERPROFILE%` and `REG_EXPAND_SZ` included; a second run adds nothing; a
  wrong `.sha256` and a missing one both refuse, with the binary unchanged.
  An update used to leave a `thirdeye.exe.old-*` behind every time, until
  the next one; the script now deletes it at once when nothing runs it.
- **A new terminal**: a tab opened from Windows Terminal's own UI, in the
  Windows Terminal that was running before the install, found
  `thirdeye 0.0.2` on its `PATH`. A tab opened by `wt.exe` from an older
  shell did not: it is most likely handed that shell's environment, so
  that is no way to check this.
- **The Claude plugin loads from thirdeye's directory**
  (`%LOCALAPPDATA%\thirdeye-data\claude`, a local marketplace Claude reads
  in place): a read, an edit, a file written by a shell command and a flag
  were one turn of `thirdeye ls`, flag first; the daemon and the run
  directory were gone when the session ended; no hook error was logged.
- **`self update` from v0.0.1 to v0.0.2 inside a running session**:
  Windows let the running `thirdeye.exe` be renamed, with a daemon and the
  MCP server running from it. The session's next edit came through the new
  binary's hook, its flag through the old binary's MCP server, both into
  the same session, and the old daemon served it to the end.
- **`self uninstall`** removed thirdeye's directory and moved the binary
  aside (`thirdeye.exe.old`, which a running program cannot delete), and
  left the logs, which go with `--purge`, and the `PATH` entry. A project
  still wired afterwards runs Claude Code normally: it ignores the plugin
  it cannot find.
- **Skills as copies**, developer mode off: `doctor` reported a project's
  copy that differed from this build's as drift, and `install -a codex`
  replaced it.
- **Two spellings of one project** (`C:\Users\…\te-win-check` and
  `c:\users\…\TE-WIN-CHECK`) were one workspace: `canonicalize` returns the
  disk's case, so both slug alike. But Claude reported its files in its own
  working directory's spelling, which every comparison downstream — all
  by component, so case-sensitive — took to be outside the root: `ls` drew
  `C:\users\…\TE-WIN-CHECK\notes.txt`, the observer's snapshot would have
  dropped it, and a shell write to it, which the observer names in the
  disk's case, would have been a second file. `Writer::append` now gives
  every path under the root the root's spelling; measured again, the read,
  the edit and the shell write were one `notes.txt`.
- **The ARM64 archive** holds an ARM64 binary (PE machine `0xAA64`); it has
  not run anywhere.
- **An interactive session in Windows Terminal, watched** (2026-11-08,
  Claude Code 2.1.319 on Opus, started from a tab opened in the UI): a read,
  an edit at line 40 of a 60-line file, a shell append and a flag. No window
  appeared before the flag; on it a separate window opened with the flagged
  file first; Enter opened the file at its lines, `d` showed the diff, Esc
  went back; `/exit` closed the window, and the run directory was empty
  afterwards. Claude recorded no hook error.
- **That session could not say whether the last `Stop` is recorded**: its
  first prompt failed at once (not logged in), and after `/login` Claude
  Code ran the same prompt again **without a `UserPromptSubmit`**, and with
  no `prompt_id` in the hooks it ran — `Stop` included, which therefore
  closed nothing. So the edits sat in a turn `ls` draws as "(no prompt
  recorded)", while the flag, whose server takes the open turn from the
  log, joined the failed attempt's turn, which never closed. Claude's
  doing, and not Windows': one hook per turn missing its id is enough.
- **An interactive session in Zellij 0.45.1, watched** (2026-11-08, in a
  Windows Terminal tab): the same four steps. The pane opened beside
  Claude on the edit, without taking focus, and behaved as the window did;
  `/exit` closed it. Every hook named its prompt, and the turn's `Stop`
  was recorded — after the flag, the last thing the session logged.
- **An interactive session in WezTerm 20240203, watched** (2026-11-08,
  `claude` started from WezTerm's default shell, cmd.exe): the same four
  steps. The pane split beside Claude, focus came back to Claude's pane,
  and `/exit` closed the pane. Every hook named its prompt, and the turn's
  `Stop` was recorded after the flag.
