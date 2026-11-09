# Security

thirdeye runs inside your agent's hooks and keeps a log that holds your
prompts and every path your agent touched. If you find a way for that data to
leak — to another user on the machine, into a world-readable file, into a
place another program trusts — or for a crafted session id, path or payload
to run a command, please report it privately rather than in a public issue:
use GitHub's **Report a vulnerability** on this repository's Security tab.

Please include the version (`thirdeye --version`), the agent and terminal
involved, and the steps to reproduce. You will get an answer within a week.

## What thirdeye does to protect your data

- Everything is local. Nothing is sent over the network.
- State is owner-only: `0700` directories and `0600` files.
- File contents are never logged, only paths, line ranges and prompts.
- Session ids, paths and workspace names from an agent are treated as
  untrusted: they are sanitized before naming any file, and passed to
  terminals as argument lists or single-quoted, never interpolated into a
  shell command.
- The hook copies a fixed allowlist of environment variables beside the log,
  never the whole environment.
