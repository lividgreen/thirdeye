---
description: Hide the thirdeye viewer until an edit may bring it back (next-mutation, next-turn, session)
argument-hint: next-mutation | next-turn | session
allowed-tools: Bash(thirdeye close:*)
---

Run `thirdeye close --until $ARGUMENTS` to hide the viewer for this session.
If no argument was given, run `thirdeye close` — it defaults to `next-turn`.

What each one means, if the human asks:

- `next-mutation` — back on the agent's very next edit, even in this turn.
- `next-turn` — back on the first edit after the next prompt. The default.
- `session` — stays hidden for the rest of the session.

A file the agent flags brings the viewer back whichever was chosen; to
silence flags too, the human wants `/thirdeye-policy never`. Inside the viewer
the same three are the keys `m`, `q` and `Q`.
