# Antigravity CLI hook payloads (verified)

Captured from `agy` 1.3.2 on 2026-10-09: a throwaway git project with a
workspace `.agents/hooks.json` on all five events, saving stdin and the
environment, run as `agy --dangerously-skip-permissions --model
gemini-3.8-flash-low -p …`, then a second turn with `--conversation <id>`.
Workspace hooks loaded in an untrusted folder.

## Configuration

```json
{ "<hook name>": {
    "PreToolUse":    [ { "matcher": "*", "hooks": [ { "type": "command", "command": "…", "timeout": 10 } ] } ],
    "PostToolUse":   [ { "matcher": "*", "hooks": [ … ] } ],
    "PreInvocation": [ { "type": "command", "command": "…" } ],
    "Stop":          [ { "type": "command", "command": "…" } ] } }
```

Keyed by a hook name of the installer's choosing, so thirdeye owns exactly
one key. Workspace: `.agents/hooks.json`; user: `~/.gemini/config/hooks.json`.

**A `PreToolUse` hook that prints a JSON object without `decision` denies
the call.** `{}` blocked every tool; printing nothing allows it.

## Payloads

**No payload names its event**, so each is registered with its name on the
command line, as for Copilot. Every payload has:

```jsonc
{ "conversationId": "c8cbdf04-de7d-4086-aec1-28d8cce95752",
  "workspacePaths": ["/abs/proj"],
  "transcriptPath": "~/.gemini/antigravity-cli/brain/<id>/.system_generated/logs/transcript_full.jsonl",
  "artifactDirectoryPath": "~/.gemini/antigravity-cli/brain/<id>",
  "modelName": "gemini-3.8-flash-low" }
```

| Event | Fields |
|---|---|
| `PreInvocation` | `invocationNum`, `initialNumSteps` — once per *model call* |
| `PostInvocation` | the same |
| `PreToolUse` | `toolCall: { name, args }`, `stepIdx` |
| `PostToolUse` | the same, plus `error` (`""` on success) |
| `Stop` | `executionNum`, `terminationReason` (`"NO_TOOL_CALL"`), `error`, `fullyIdle` |

- **There is no prompt text and no session end.** The transcript has the
  prompt; thirdeye does not read it.
- **A turn** starts at the `PreInvocation` with `invocationNum: 0`, and
  ends at `Stop`, once per turn. `invocationNum` restarts at 0 for each
  prompt. `initialNumSteps` at that point is the user message's step index
  (1, then 21), so it is unique within the conversation. `executionNum` was 0
  for both turns, so it is not an id.
- **`stepIdx` is shared by a call's two edges and unique within the
  conversation**: a real call id.
- **A failed tool can have no `PostToolUse`.** A `view_file` of a missing
  file had only its `PreToolUse`.
- A command exiting non-zero still has `error: ""`.

## Tools seen

| `name` | `args` |
|---|---|
| `view_file` | `{ "AbsolutePath", "StartLine"?, "EndLine"? }`, inclusive lines |
| `replace_file_content` | `{ "TargetFile", "StartLine", "EndLine", "TargetContent", "ReplacementContent", "AllowMultiple", … }`; `StartLine`/`EndLine` are the replaced region, before the edit |
| `write_to_file` | `{ "TargetFile", "CodeContent", "Overwrite", … }` |
| `run_command` | `{ "CommandLine", "Cwd", "IsDaemon", "WaitMsBeforeAsync", … }` — searching and listing went through it (`grep -rn`, `ls -la`) |
| `call_mcp_tool` | `{ "ServerName": "thirdeye", "ToolName": "flag", "Arguments": { … } }` |

Every call also carries `toolAction`/`toolSummary` (UI text). The
replaced and written content (`ReplacementContent`, `CodeContent`) must not
be logged.

Before calling an MCP tool the model `view_file`s its schema,
`~/.gemini/antigravity-cli/mcp/<server>/<tool>.json`: the agent loading its
own tools, not reading the project.

## Environment

The hook runs as `sh -c <command>`, a child of the `agy` process, **with
the directory holding `hooks.json` as its cwd** (`<proj>/.agents`). It
inherits the terminal's environment (a marker on the command line reached
it), so surfaces work. Antigravity adds `ANTIGRAVITY_CONVERSATION_ID`.

## MCP servers

`.agents/mcp_config.json` or `~/.gemini/config/mcp_config.json`, `{
"mcpServers": { "<name>": { "command", "args"?, "env"?, "cwd"? } } }`. The
server starts **in the project directory**, with the terminal's environment
but **no conversation id**, so a flag joins the workspace's active session.
MCP tools ask before every call unless `permissions.allow` in
`~/.gemini/antigravity-cli/settings.json` (global only) lists
`mcp(<server>/<tool>)`.

## Skills

`.agents/skills/<name>/SKILL.md` in the workspace,
`~/.gemini/antigravity-cli/skills/<name>/` for the user (from the docs).
