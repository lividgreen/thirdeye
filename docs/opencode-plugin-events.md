# OpenCode plugin events (verified)

Captured on 2026-10-08 from OpenCode 2.0.24 (model
`opencode/mimo-v2.6-flash-free`) with two throwaway plugins in a scratch
project's `.opencode/plugins/`, run as `opencode run --standalone --auto`.
Raw captures are **not** committed — they contain file contents. This is the
OpenCode counterpart of `docs/hook-payload.md` and `docs/codex-hook-payload.md`.

## There are no hooks — there is a plugin

OpenCode 2 has no shell-command hooks. Integration is a plugin: a JS module
the OpenCode **server** loads, discovered from `<project>/.opencode/plugins/
<name>/index.js` or `~/.config/opencode/plugins/<name>/index.js`.

- **A plain JS default export works**; nothing needs importing from
  OpenCode's packages:

  ```js
  export default { id: "thirdeye", async setup(ctx) { /* register */ } }
  ```

- `ctx` has, among others: `location`, `session`, `tool`, `event`, `shell`,
  `mcp`, `permission`, `skill`. `ctx.location.directory` is the project root.
- The plugin runs **in the server process**, long-lived, for every session the
  server serves — so it can keep state (the current turn per session) that a
  shell hook could not.

## The prompt — a turn opens

`ctx.session.hook("prompt", e => …)`:

```jsonc
{
  "sessionID": "ses_ee488db48ffe0eyfWeMfffduLL",
  "messageID": "msg_11b7725ea0010lIK36KW4XQzDI",   // the user message: the turn id
  "prompt": { "text": "…", "files": [] },
  "delivery": "steer"
}
```

## Tools — `execute.before` / `execute.after`

`ctx.tool.hook("execute.before" | "execute.after", e => …)`. The same `id`
appears in both and in the `session.tool.*` events.

```jsonc
// before
{ "tool": "edit", "sessionID": "ses_…", "agent": "build",
  "messageID": "msg_…",            // the ASSISTANT message — changes every step
  "id": "call_8cd06120f2844cd58c34939b",
  "input": { "path": "data.txt", "oldString": "beta", "newString": "BETA" } }
// after: the same, plus
  "status": "completed",
  "result": { "output": …, "content": [ … ] }
```

| `tool` | `input` | `result.output` |
|---|---|---|
| `read` | `{ "path": "data.txt" }` (relative) | `{ "type": "file", "uri": "file:///abs/…", "content": "<whole file>" }` |
| `write` | `{ "path", "content" }` | `{ "operation": "write", "target": "/abs/…", "existed": false }` |
| `edit` | `{ "path", "oldString", "newString" }` | `{ "files": [{ "file", "patch": "<unified diff>", "status": "modified" }] }` |
| `shell` | `{ "command": "printf 'x\\n' > shell.txt" }` | `{ "exit": 0, "output": "<stdout+stderr>" }` |
| `grep` | `{ "pattern": "TODO", "path": "src" }` (`path` optional) | `[{ "entry": { "path", "type" }, "line", "text", "submatches" }]` |
| `glob` | `{ "pattern": "**/*.rs" }` (`path` optional) | `[{ "path": "src/main.rs", "type": "file" }]` |

`grep` and `glob` were captured from 2.0.24 on 2026-10-09; a search without
`path` covers the session's directory. There is no listing tool.

- **`result` embeds file contents and command output.** Never log it.
- `edit`'s patch carries hunk headers (`@@ -1,3 +1,3 @@`): the edit's line
  range, in new-file coordinates.
- **`messageID` on a tool event is the assistant's message, not the user's.**
  It changes every step, so it is not the turn. The plugin remembers the
  prompt's `messageID` per session and attaches it.
- **`execute.before` is awaited.** An async hook that waited 300 ms delayed
  the shell command until it returned: a bracket's start marker lands before
  the command runs.
- **Code Mode wraps calls.** A tool call may arrive as `tool: "execute"` with
  the nested tool firing its own hooks under the same `id`. File tools report
  themselves; only `shell` needs bracketing.

## A plugin-defined tool

`ctx.tool.transform(editor => editor.add({ name, description, input, execute }))`.
`execute(input, context)` receives:

```jsonc
{ "sessionID": "ses_…", "agent": "build", "messageID": "msg_…",
  "id": "call_…", "progress": <fn>, "signal": <AbortSignal> }
```

So a flag tool defined by the plugin knows its exact session — no MCP server,
no active-session guess, no `opencode.json` to merge.

## Server events — turns close, the server ends

`for await (const ev of ctx.event.subscribe({ signal }))`:

- `session.execution.started` / `session.execution.succeeded` /
  `session.execution.failed` — `{ "data": { "sessionID": "ses_…" } }`. A turn
  ends with one of the last two.
- `location.shutdown` — when the server shuts down. There is no per-session
  end event; under `--standalone` this is the session ending.
- Many more (`session.step.*`, `session.tool.*`, `shell.created`…), unused.

## Environment

The plugin inherits the **server's** environment. Under `--standalone` that
is the terminal's (the capture saw `TERM_PROGRAM=ghostty`). The default is a
shared background service (`~/.config/opencode/service.json`), whose
environment is whatever started it — so `TMUX` and `KITTY_LISTEN_ON` may
belong to another terminal, or be absent. Not yet verified.
