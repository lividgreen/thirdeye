# pi extension events (verified)

Captured on 2026-10-08 from pi 1.1.0 (model `openai-codex/gpt-6-luna`) with a
throwaway extension loaded by `pi -p --no-extensions -e capture.js`, and read
against the type declarations pi ships
(`dist/core/extensions/types.d.ts`). Raw captures are **not** committed —
they contain file contents.

## An extension, in the agent's own process

pi loads TypeScript or JavaScript extensions: a default-exported factory that
receives the extension API. Locations: `~/.pi/agent/extensions/<name>/index.js`
(user) and `.pi/extensions/<name>/index.js` (project, after the project is
trusted). Skills: `~/.pi/agent/skills/`, `.pi/skills/`.

**It runs in the pi process**, so it inherits the terminal's environment —
`TMUX`, `KITTY_LISTEN_ON`, `WEZTERM_*` reach the hook, and surfaces work as
they do for Claude Code. (Contrast OpenCode, whose plugin runs in a shared
server.)

```js
export default function (pi) {
  pi.on("tool_call", async (event, ctx) => { /* … */ });
}
```

Every handler's `ctx` has `ctx.sessionManager.getSessionId()` — a UUID such
as `01a11c12-aaea-73bc-abec-5b138d399814` — and `ctx.cwd`.

## Events

| Event | Fields used | Meaning |
|---|---|---|
| `session_start` | `reason`: `startup`/`reload`/`new`/`resume`/`fork` | a session became current |
| `before_agent_start` | `prompt` (also `systemPrompt` — large, never forwarded) | a turn opens |
| `tool_call` | `toolName`, `toolCallId`, `input`, `parentToolCallId?` | before a tool runs; **awaited** |
| `tool_result` | the same, plus `content`, `isError`, `details` | after it ran |
| `agent_settled` | `aborted` | the turn is over; pi will not continue on its own |
| `session_shutdown` | `reason`: `quit`, or a switch to another session | the session ends |

- **There is no turn id.** The extension mints one at `before_agent_start`.
- **`tool_call` is awaited**: a handler delaying 300 ms held `bash` until it
  returned, so a bracket's start marker lands before the command runs.
- `toolCallId` looks like `call_…|fc_…`; nested calls made by tools get
  `<parent>/<n>` and a `parentToolCallId`.

## Tools

| `toolName` | `input` | in `tool_result` |
|---|---|---|
| `read` | `{ "path": "data.txt", "offset": 1, "limit": 10 }` | `content` = the file text |
| `write` | `{ "path": "notes.md", "content": "…" }` | — |
| `edit` | `{ "path": "data.txt", "edits": [{ "oldText", "newText" }] }` | `details.patch`: a unified diff with `@@ -a,b +c,d @@` hunks; `details.firstChangedLine` |
| `bash` | `{ "command": "printf 'x\\n' > shell.txt" }` | `structuredContent`: `{ output, exit_code, … }` |
| `grep` | `{ "pattern": "TODO", "path": "src" }` (`path` optional) | — |
| `find` | `{ "pattern": "*.rs" }` (`path` optional) | — |
| `ls` | `{ "path": "src" }` (`path` optional) | — |

`grep`, `find` and `ls` were captured from 1.1.0 on 2026-10-09. They are
**off by default**: pi enables only `read`, `bash`, `edit` and `write`
unless told `--tools +grep,+find,+ls`. A missing `path` means the session's
directory. Also built in, not captured: `powershell`.

**`content` and `structuredContent` carry file contents and command output.
Never log them.**

## A tool the extension registers

`pi.registerTool({ name, label, description, parameters, execute })` with a
plain JSON-Schema `parameters` works. `execute(toolCallId, params, signal,
onUpdate, ctx)` — and `ctx.sessionManager.getSessionId()` is the exact
session, so a flag tool needs no guessing.
