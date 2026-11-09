---
description: Set when the thirdeye viewer may open itself (any, attention, not-this-turn, never)
argument-hint: any | attention | not-this-turn | never
allowed-tools: Bash(thirdeye policy:*)
---

Run `thirdeye policy $ARGUMENTS` to set the reopen policy for this session.

The modes, if the human asks or gives none:

- `any` — a mutation or a flag opens the viewer. The default.
- `attention` — only a file the agent explicitly flagged opens it.
- `not-this-turn` — suppressed until the next prompt.
- `never` — only `/thirdeye-open` opens it.

Events are recorded in every mode; this controls visibility only.
