# Claude Code hook payloads (verified)

Captured empirically on 2026-08-01, Claude Code with `effortLevel: high`, by
installing a `PostToolUse` + `UserPromptSubmit` hook that dumped stdin.
Method: `.claude/spike/dump-hook.sh` (throwaway) + a headless
`claude -p` run. Raw captures are **not** committed (they contain file
contents).

> Project-local `.claude/settings.json` is read at **session start**. Editing
> it mid-session does not activate new hooks — the spike only produced output
> once a fresh `claude` process ran. `thirdeye hook install` must therefore
> tell the user to restart their agent.

## PostToolUse

```jsonc
{
  "session_id":      "328abcfc-2bd5-4aa9-8aa8-07d14569d550",
  "transcript_path": "/home/…/.claude/projects/<slug>/<session_id>.jsonl",
  "cwd":             "/home/u/projects/app",
  "prompt_id":       "838748f2-65af-4e75-948b-5629215d63b9",
  "permission_mode": "default",
  "hook_event_name": "PostToolUse",
  "tool_name":       "Read",
  "tool_input":      { "file_path": "/abs/path" },
  "tool_response":   { "type": "text", "file": {
                         "filePath":  "/abs/path",
                         "content":   "…",
                         "numLines":  8,
                         "startLine": 1,
                         "totalLines": 8 } },
  "tool_use_id":     "toolu_011NdbU3kC4B29Gh2uqYTvJJ",
  "duration_ms":     1,
  "effort":          { "level": "high" }
}
```

## PostToolUse (Edit)

Captured live from an `Edit` `PostToolUse` hook (2026-08-01, same method as
above). `tool_response` for `Edit`/`MultiEdit` has **no `file` object** —
that shape is `Read`-only. Its top-level keys are `filePath`, `newString`,
`oldString`, `originalFile`, `replaceAll`, `structuredPatch`,
`userModified`:

```jsonc
{
  "session_id":      "…", "transcript_path": "…", "cwd": "…",
  "prompt_id":       "…", "permission_mode": "default",
  "hook_event_name": "PostToolUse",
  "tool_name":       "Edit",
  "tool_input":      { "file_path": "/abs/path", "old_string": "…", "new_string": "…" },
  "tool_response":   {
                         "filePath":       "/abs/path",
                         "newString":      "…",
                         "oldString":      "…",
                         "originalFile":   "…",
                         "replaceAll":     false,
                         "userModified":   false,
                         "structuredPatch": [
                           { "oldStart": 1, "oldLines": 5,
                             "newStart": 1, "newLines": 5,
                             "lines": [" line one", " line two",
                                       "-line three", "+line THREE",
                                       " line four", " line five"] }
                         ]
                       },
  "tool_use_id":     "…",
  "duration_ms":     1
}
```

`structuredPatch` is a **list** of hunks — `MultiEdit` produces one hunk per
edited region. Each hunk carries both old- and new-file coordinates
(`oldStart`/`oldLines`, `newStart`/`newLines`); the touched range for the
log should come from the *new*-file coordinates, since that is what should
be reachable when a human jumps to what changed.

## UserPromptSubmit

```jsonc
{
  "session_id": "…", "transcript_path": "…", "cwd": "…",
  "prompt_id": "838748f2-…", "permission_mode": "default",
  "hook_event_name": "UserPromptSubmit",
  "prompt": "Read the file .claude/spike/dump-hook.sh and reply with just the word DONE"
}
```

## Stop

`Stop` **does carry `prompt_id`**, matching the `UserPromptSubmit` that opened
the turn. Verified end to end on 2026-08-01: a real session wrote a
`turn:open` and a `turn:close` record sharing
`prompt_id d5af4aa0-5c5c-40ab-9cce-d0f274b34ce0`, and `thirdeye ls` rendered
the turn as closed.

This was the last payload fact the capture pipeline assumed rather than
verified, and it was load-bearing: without `prompt_id` on `Stop`, no turn
would ever close and every turn would render as current forever.

`session_id` and `cwd` are present too — the close marker carried the session
id, and the workspace resolved from `cwd` without an explicit flag.

## Consequences for thirdeye

1. **`tool_use_id` is present.** The plan treated exact dedup as a
   best-effort fallback behind a timing-based precedence rule. It is not —
   hook events and transcript `tool_use` blocks carry the *same* id, so
   dedup is an exact set-membership test. The timing heuristic is downgraded
   to a safety net for events that genuinely lack an id.

2. **`session_id` and `cwd` are always present.** Session-keyed log routing
   needs no inference for the push path; `_unattributed` is reachable only
   from pull adapters.

3. **`tool_response.file.startLine` / `numLines` give the touched range
   directly — but only for `Read`.** This was originally generalized to
   "all tools" from a capture that only ever covered `Read`; a live capture
   of `Edit` showed that assumption was wrong. `Edit`/`MultiEdit` carry no
   `file` object at all — the range instead comes from
   `tool_response.structuredPatch`, a list of hunks (one per edited region),
   using each hunk's `newStart`/`newLines`. Either shape is correct for its
   own tool family; neither generalizes to the other.

4. **`prompt_id` groups tool calls under the prompt that caused them.** Not
   in the original design; it enables a "files touched answering *this*
   request" grouping in the TUI. Recorded as an opportunity, not yet built.

5. **`tool_response` embeds full file content.** thirdeye must discard it —
   events stay small (keeps log lines well under `PIPE_BUF`) and the log
   must not become a second copy of the user's source tree.
