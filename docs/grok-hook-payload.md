# Grok Build hook payloads (verified)

Captured from grok 1.0.50 on 2026-10-09 and 2026-10-10: a throwaway git
project with a hook on every event that saved stdin and the environment,
run headless (`grok --yolo -p`, `GROK_FOLDER_TRUST=0` so project hooks load
untrusted). The lifecycle events are from Grok's own model; the tool events
are from a full run on another model through a custom-model entry
(`kimi-k2.7-code`), which changes who chooses the tool calls and nothing
about the harness that reports them.

## Every payload

```jsonc
{ "hookEventName": "user_prompt_submit",      // grok's snake_case name
  "hook_event_name": "UserPromptSubmit",      // Claude's PascalCase name
  "sessionId": "01a1200f-ffa7-7e01-9cc7-0e4e8112e3e3",
  "session_id": "01a1200f-ffa7-7e01-9cc7-0e4e8112e3e3",
  "cwd": "/abs/proj",
  "workspaceRoot": "/abs/proj/",              // note the trailing slash
  "timestamp": "2026-10-09T09:48:01.953190456+00:00",
  "permissionMode": "bypassPermissions",
  "permission_mode": "bypassPermissions",
  "promptId": "e94ec90a-77b2-41fa-8fb2-08c39eab3326",   // the prompt and its Stop only
  "prompt": "…" }                                        // UserPromptSubmit only
```

- **Several keys are repeated in Claude's snake_case:** `hook_event_name`,
  `session_id`, `transcript_path`, `permission_mode`, and on tool events
  `tool_name`, `tool_input`, `tool_use_id` and `tool_response`. `promptId`
  is not. A payload sent to the Claude adapter is therefore accepted, and
  yields no events only because Grok's tool names are not Claude's. See
  "Claude compatibility".
- **The prompt text is `prompt`**, on `UserPromptSubmit`. The docs did not
  name it.
- `transcriptPath` (and `transcript_path`) appear on `Notification`,
  `StopFailure`, `Stop` and `SessionEnd`, pointing into
  `~/.grok/sessions/<url-encoded cwd>/<session>/`.

## Lifecycle, as observed

One headless turn whose model call failed (HTTP 429), in order:

| Event | Fields beyond the common ones |
|---|---|
| `SessionStart` | `source: "new"` |
| `UserPromptSubmit` | `promptId`, `prompt` |
| `Notification` | `notificationType: "agent_error"`, `message`, `level` |
| `StopFailure` | `promptId`, `error: "rate_limit"`, `errorDetails`, `lastAssistantMessage` |
| `SessionEnd` | `reason: "shutdown"` |
| `Stop` | `reason: "shutdown"`, `stopHookActive: false`, **no `promptId`** |

- **A `Stop` arrives after `SessionEnd`, carrying no `promptId`.** It is the
  session-end "observe fire" the docs mention, not a turn ending. A turn
  closes on `Stop`, `StopFailure` or `StopCancelled` *with* a `promptId`.
  Anything without one closes nothing.
- A turn whose model call fails ends with `StopFailure`, not `Stop`.

## Environment

The hook runs as `sh -c <command>`, a child of the `grok` process; leader
mode is off by default. It inherits the terminal's environment (a marker
variable set on the `grok` command line reached it, as did `TERM_PROGRAM`),
so surfaces work as for Claude. Grok adds:

| Variable | Value |
|---|---|
| `GROK_HOOK_EVENT` | `session_start`, … — set only on hook processes |
| `GROK_HOOK_NAME` | `project/capture:session_start[0].hooks[0]`, or `project/settings:…` for a `.claude/settings.json` hook |
| `GROK_SESSION_ID` | the session id |
| `GROK_WORKSPACE_ROOT` | `/abs/proj/` (trailing slash) |
| `CLAUDE_PROJECT_DIR` | the same, trailing slash included |

`CLAUDE_PROJECT_DIR` is what `workspace::resolve` prefers. The trailing
slash is harmless only because the root is canonicalized.

## Claude compatibility

Grok runs the hooks in `.claude/settings.json` (project, once trusted) and
`~/.claude/settings.json`, handing them **the same payload** its own hooks
get, not a Claude-shaped one. It does **not** run hooks from Claude Code
*plugins*: a session in a project with thirdeye's Claude plugin enabled left
nothing in thirdeye's log.

So a `thirdeye hook` (Claude) written into a settings file is run by Grok
too. Unguarded, it accepted the payload and marked Grok's session as the
workspace's `active-session`. Since dc6e92a the Claude path does nothing when
`GROK_HOOK_EVENT` is set.

## Tool events

```jsonc
// PreToolUse — the common fields, plus:
{ "toolName": "read_file",
  "toolUseId": "read_file_0_6638d15e",       // "call-<uuid>-0" on Grok's own model
  "toolInput": { "target_file": "src/calc.py" },
  "toolInputTruncated": false,
  "tool_name": "read_file", "tool_input": { … }, "tool_use_id": "…" }
// PostToolUse — the same, plus:
  "toolResult": { "type": "ReadFile", "FileContent": { "content": "…", … } },
  "toolResultTruncated": false, "durationMs": 3, "isBackgrounded": false,
  "tool_response": { … }, "duration_ms": 3
```

- **Tool events carry no `promptId`**, whatever Grok's docs say: only
  `UserPromptSubmit` and the turn's `Stop` do. The turn has to be held from
  one to the other.
- **`toolUseId` is on both edges and matches**: a real call id.
- **`toolResult` holds file contents and command output.** Only its shape is
  read, never its text.
- **Tool events repeat Claude's snake_case fields** (`tool_name`,
  `tool_input`, `tool_use_id`, `tool_response`). The tool names are Grok's
  own, so the Claude adapter records nothing from them; only the guard in
  `cmd_hook.rs` stands between a renamed tool and a double count.

| `toolName` | `toolInput` | `toolResult` |
|---|---|---|
| `read_file` | `{ "target_file" }`, relative to `cwd` or absolute | `{ "type": "ReadFile", "FileContent": { "content", "absolute_path", "offset", "total_lines" } }`; a missing file is `{ "FileNotFound": "Error: …" }`, **still a `PostToolUse`** |
| `search_replace` | `{ "file_path", "old_string", "new_string" }` | `{ "type": "SearchReplace", "EditsApplied": { "absolute_path", "edits": { "details": [{ "old_line", "new_line", "old_string", "new_string", … }] } } }` |
| `write` | `{ "file_path", "content" }` | the same `EditsApplied` shape, `old_string: ""`, `new_line: 1` |
| `grep` | `{ "pattern", "path" }` | `{ "type": "GrepSearch", "stdout": [bytes], "match_count", … }` |
| `list_dir` | `{ "target_directory" }` | `{ "type": "ListDir", "Content": { … } }` |
| `run_terminal_command` | `{ "command", "description" }` | `{ "type": "Bash", "exit_code", "output", "current_dir", … }` — a non-zero exit is still a `PostToolUse` |
| `search_tool` | `{ "query" }` | the model looking for a tool, e.g. an MCP one |
| `thirdeye__flag` | `{ "tool_name": "thirdeye__flag", "tool_input": { "path", "reason" } }` | an MCP tool, named `<server>__<tool>` |

`new_line` and the length of `new_string` give an edit's range in the new
file.

A completed turn ends with `Stop` carrying `promptId`, `reason: "end_turn"`
and `lastAssistantMessage`; the `Stop` after `SessionEnd` carries no
`promptId`.

## Plugins and MCP

- **A plugin is installed with `grok plugin install <path> --trust`, per
  user, and copied** into `$GROK_HOME/installed-plugins/<dir>-<hash>/`. A
  directory merely placed in `$GROK_HOME/plugins/` was not loaded.
- `grok plugin list` prints `  <id>: <name> [local: <source path>]`, where
  the id is `<dir>-<hash>` and the name comes from `plugin.json`. **`grok
  plugin uninstall` takes the name**; given the id it answers "not found".
  It removes one copy per call, and two installs from different paths can
  share a name.
- **Installing a plugin makes Grok rewrite `config.toml`**: it adds
  `[plugins] enabled`, drops comments and turns inline tables into
  sub-tables. Uninstalling leaves `[plugins] enabled` behind.
- MCP tools ask before their first use in a project unless
  `[permission] allow` lists `MCPTool(<server>__<tool>)`.
- Its `hooks/hooks.json` (Claude's format) fires, with `GROK_PLUGIN_ROOT`
  set, and its `.mcp.json` (`{ "mcpServers": { … } }`) starts its servers.
- **An MCP server starts in the project directory with `GROK_SESSION_ID`
  set** to the session's id, and the terminal's environment.
- Without a flag tool, a model told to flag found `thirdeye` on `PATH` and
  ran `thirdeye flag` from its shell with a session id it guessed from the
  environment.

## Not captured

- `PostToolUseFailure` (a dispatch failure or an MCP error), `StopCancelled`
  (an interrupt), subagents, and `run_terminal_cmd`, the shell tool's name in
  Grok's headless docs; the hook that fired said `run_terminal_command`.
- Hooks are synchronous; the default timeout is 5 s (from the docs).
