# GitHub Copilot CLI hook payloads (verified)

Captured from Copilot CLI 1.0.94 on 2026-10-09: a throwaway git project, a
plugin loaded with `--plugin-dir` whose `hooks.json` registered every event
in both spellings and saved stdin and the environment, run with `copilot -p
… --allow-all`. The model was "auto", which resolved to
`mai-code-1.1-flash`; the account used offered no other model to `-p`.

## Where hooks load from

- **Plugin hooks fire.** `hooks.json` at the plugin root, `{ "version": 1,
  "hooks": { "<event>": [ { "type": "command", "bash": "…", "timeoutSec": 10
  } ] } }`.
- **Repository hooks did not**, neither `.github/hooks/*.json` nor
  `.claude/settings.json`, in a fresh git repository under `-p`. The docs
  name no trust gate for hooks, but Copilot keeps a folder-trust list (not a
  user setting: `trustedFolders` in `~/.copilot/settings.json` is reported
  as unknown and ignored). Presumably an untrusted folder skips them;
  unverified.

## The native (camelCase) payloads

**No payload names its event**, except `permissionRequest`'s `hookName`.
The event has to be passed by the command that the hook config runs.

| Event | Fields (all have `sessionId`, `timestamp` in epoch ms, `cwd`) |
|---|---|
| `userPromptSubmitted` | `prompt` |
| `userPromptTransformed` | `prompt`, `transformedPrompt` (the prompt with a `<current_datetime>` prefix) |
| `sessionStart` | `source: "new"`, `initialPrompt` |
| `preToolUse` | `toolName`, `toolArgs` |
| `permissionRequest` | `hookName`, `toolName`, `toolInput`, `permissionSuggestions` — fires even under `--allow-all` |
| `postToolUse` | `toolName`, `toolArgs`, `toolResult: { resultType, textResultForLlm }` |
| `postToolUseFailure` | `toolName`, `toolArgs`, `error` |
| `agentStop` | `transcriptPath`, `stopReason: "end_turn"`, `stop_hook_active` |
| `sessionEnd` | `reason: "complete"` |

- **`userPromptSubmitted` fires before `sessionStart`.**
- **There is no tool-call id and no turn id**, on any event.
- **`toolResult.textResultForLlm` is the file's content for `view`.** Never
  log it.
- A command that exits non-zero is a `postToolUse` with `resultType:
  "success"`: `<shellId: 2 completed with exit code 1>`. A failed `view`
  (missing file) is a `postToolUseFailure`.
- `sessionId` here is the session `--resume` takes, and the
  `COPILOT_AGENT_SESSION_ID` an MCP server gets.

## Tools seen

| `toolName` | `toolArgs` |
|---|---|
| `view` | `{ "path": "/abs/src/calc.py" }` — absolute |
| `apply_patch` | a **string**: `*** Begin Patch\n*** Update File: src/calc.py\n@@\n …\n*** End Patch\n`; `*** Add File: src/div.py` for a new file. Paths relative to `cwd`. Hunks are context-anchored (`@@` with no line numbers), so they give no range |
| `rg` | `{ "pattern": "return", "output_mode": "content" }` (`path` optional) |
| `glob` | `{ "pattern": "**/*.py" }` |
| `bash` | `{ "command": "sed -i 's/world/there/' notes.txt", "description": "…" }` |

`permissionRequest` names the edit tool `edit` with `toolInput: {
"file_path", "diff" }`, even though the call is `apply_patch`.

**The tool set depends on the model.** The docs list `bash`, `view`,
`create`, `edit`, `grep`, `glob` and `apply_patch`; this model used
`rg` (not `grep`) and `apply_patch` (not `edit`/`create`). Other models'
shapes have not been captured yet.

## The Claude-compatible (PascalCase) payloads

Registering `PreToolUse` and the like in the same plugin yields a second
payload per event: snake_case fields, `hook_event_name`, an ISO
`timestamp`, Claude's tool names (`Read`, `Edit` for `apply_patch`, `Grep`
for `rg`, `Glob`, `Bash`), and **the same arguments**: `Read`'s input is
`{ "path" }`, not `{ "file_path" }`, and `Edit`'s is the patch string. There
is still no `tool_use_id` or `prompt_id`. A failed `view` is
`PostToolUseFailure`, and `Stop` carries `stop_reason`.

So the Claude adapter, fed one of these, records only `Grep`/`Glob` as
searches, filed as Claude's, and marks Copilot's session as the workspace's
active one.

## Environment

Hooks run as children of Copilot's own process and inherit the terminal's
environment, so surfaces work as for Claude. Copilot adds:

| Variable | Hooks | Shell tool |
|---|---|---|
| `COPILOT_CLI=1`, `COPILOT_CLI_BINARY_VERSION` | yes | **yes** |
| `COPILOT_AGENT_SESSION_ID` | no | yes (and MCP servers: the session id) |
| `COPILOT_PROJECT_DIR`, `CLAUDE_PROJECT_DIR` | the project root | **no** |
| `COPILOT_PLUGIN_ROOT`, `CLAUDE_PLUGIN_ROOT`, `PLUGIN_ROOT` | the plugin's directory | no |
| `COPILOT_PLUGIN_DATA`, `CLAUDE_PLUGIN_DATA` | `~/.copilot/plugin-data/_direct/<hash>` | no |

## MCP servers in a plugin

A plugin's `.mcp.json` takes Claude's shape, `{ "mcpServers": { "thirdeye": {
"command", "args" } } }`. The server starts **with the plugin's directory as
its working directory**, with `COPILOT_AGENT_SESSION_ID` set and no project
variable, so it cannot locate the project from its own environment. Hooks
see its tool as `thirdeye-flag` (`<server>-<tool>`), and so does the model.

## A plugin in a local marketplace

`copilot plugin marketplace add <dir>` (with `.github/plugin/marketplace.json`
in `<dir>`) and `copilot plugin install <plugin>@<marketplace>` reply: "It is
loaded live from <dir>/<plugin>, so edits take effect on the next session —
nothing was copied." `copilot plugin uninstall` of such a plugin only
disables it; removing the marketplace drops it.

## Which variable marks a hook

`COPILOT_PROJECT_DIR` is the one a Claude hook can tell Copilot apart by:
a Claude Code session started from Copilot's shell does not inherit it. It
was seen on plugin hooks; that settings-file hooks get it too is assumed.

## Claude's plugins, from a project's Claude settings

Captured from Copilot CLI 1.0.98 on 2026-10-16, with a stand-in plugin in
a directory marketplace that a scratch project's
`.claude/settings.local.json` declared (`extraKnownMarketplaces`) and
enabled (`enabledPlugins`) — the shape `thirdeye install -a claude` writes.

- **Copilot loads it.** Its changelog says so (1.0.12: "Read
  .claude/settings.json and .claude/settings.local.json as additional repo
  config sources"), and an **interactive** session in the project started
  the plugin's MCP server and offered its skill. Two `copilot -p` runs in
  the same project loaded nothing of it.
- **A user-installed plugin of the same name wins.** With thirdeye's
  Copilot plugin installed (`thirdeye@thirdeye` from the `thirdeye`
  marketplace, the same ids as Claude's), the project's one was not loaded
  at all: neither its hooks nor its server ran.
- **What the plugin's MCP server gets:** `COPILOT_PLUGIN_ROOT`,
  `COPILOT_LOADER_PID`, `CLAUDE_PLUGIN_ROOT`, `PLUGIN_ROOT`,
  `COPILOT_AGENT_SESSION_ID`, `COPILOT_CLI_BINARY_VERSION`,
  `COPILOT_CLI_RESOLVED_DIST_DIR`. **Copilot's shell commands get**
  `COPILOT_AGENT_SESSION_ID`, `COPILOT_CLI`, `COPILOT_CLI_BINARY_VERSION`
  and `COPILOT_CLI_RESOLVED_DIST_DIR` — so a Claude Code started from one
  inherits Copilot's session id, but not `COPILOT_PLUGIN_ROOT`.

So `thirdeye mcp` serving as Claude's stands aside when
`COPILOT_PLUGIN_ROOT` is set (`CLAUDE_SERVER_BORROWERS` in `cmd_mcp.rs`): it
lists no tool. Before it did, a flag raised there was filed as Claude's, in
whatever session the workspace had last. Verified after the change: in an
interactive session the server connected, the model found no `flag` tool,
and nothing was written to any log.

