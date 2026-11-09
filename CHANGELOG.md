# Changelog

All notable changes to thirdeye are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Until 1.0, a minor
version may change the log format, the head protocol or the command line.

## 0.0.1 — 2026-11-09

The first release.

### Agents

- Claude Code, Codex CLI, OpenCode, pi, GitHub Copilot CLI, Antigravity CLI
  and Grok Build. thirdeye records reads, edits with their line ranges,
  writes, searches, files written or deleted by shell commands, and turns.
- A `flag` tool for every agent, with a skill that tells the agent when to
  use it, so that it can point you at a file with a reason.

### Viewer

- A terminal viewer that lists the session's files by turn, ranked by how
  much the agent did with each, and opens a file at the lines the agent
  touched or shows its diff.
- It opens by itself beside the agent: a pane in tmux, Zellij and WezTerm, a
  window in kitty's tab, and a separate window in Ghostty, Alacritty, GNOME
  Terminal and Windows Terminal. A policy decides when it may open, and the
  human can hide it until the next edit, the next turn or the session's end.

### Installation

- `install.sh` and `install.ps1`, which check the archive's SHA-256.
- `thirdeye install`, `uninstall`, `doctor` and `refresh`, which wire
  thirdeye into each agent's own configuration, and remove exactly what they
  added.
- `thirdeye self update` and `thirdeye self uninstall`.
- systemd user units on Linux; on Windows, the hook starts the daemon itself.

### Platforms

- Linux, with or without systemd, and WSL 2.
- Windows 10, for Claude Code in Windows Terminal, Zellij and WezTerm.
