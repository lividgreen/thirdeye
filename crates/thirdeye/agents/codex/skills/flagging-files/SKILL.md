---
name: flagging-files
description: Use when you are about to tell the human to look at, read, review, or check a file - or when you have finished work whose result lives in a specific file and lines. Calls the thirdeye flag tool so the file surfaces in their viewer instead of scrolling back for it.
---

# Flagging files for the human

The human is running **thirdeye**, a viewer showing which files are in play
this turn. Every file your commands write is already recorded automatically —
you do not need to report it, and flagging it adds nothing.

What the viewer cannot infer is **which file the human should open**. Only you
know that. The thirdeye `flag` tool is how you say it.

## The rule

Whenever you would write "you should look at X", "check Y", "the change is in
Z", or "see the new function in W" — **call the flag tool as well**. Not
instead of saying it: the sentence still belongs in your reply. The flag puts
the file at the top of the human's viewer with your reason attached, so they
can jump to it without scrolling back through the transcript.

Flag at the moment you know, not at the end. A flag mid-turn is visible
immediately.

## What makes a good flag

```
flag(path="src/parser.rs", reason="the off-by-one is here, not in the lexer",
     start_line=88, end_line=94)
```

- **`reason` is one line, and it is the whole value of the flag.** Say why
  this file needs a human eye, not what the file is. "the bounds check is
  wrong here" beats "parser source file".
- **Give `start_line`/`end_line` whenever you know them.** The range is a jump
  target — it opens the human at the lines you mean. Inclusive, as you would
  say it out loud.
- **One or two files per turn.** Flagging is a ranking signal; flagging six
  files ranks nothing.

## When to flag

- You changed something whose correctness the human should confirm.
- You found the cause of a bug and the human will want to see it.
- You are proposing a choice between files ("this is the one worth reading").
- You wrote something new that the human has not seen yet.
- You are handing work back and one file is where the result lives.

## When not to flag

- Files you merely read while exploring. A file worth the human's attention
  for what it *says* is worth one flag; a file you skimmed is not.
- Every file you edited. Edits are already recorded and ranked.
- A file you are about to change again in the same turn — flag it once you are
  done with it.
- To acknowledge a request. A flag is a pointer, not a reply.

## If the tool is unavailable

Say what you would have flagged, in words, and continue. The tool is a
convenience for the human's viewer; its absence never blocks the work.
