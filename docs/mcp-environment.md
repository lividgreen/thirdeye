# What Claude Code hands an MCP server (verified)

Captured empirically on 2026-08-01 against Claude Code 2.1.220, by registering
a throwaway stdio MCP server that dumped its environment, its `initialize`
params and its `tools/call` params, then driving it with a headless
`claude -p` run. The spike server is gone; this file is the record.

Same standing as `docs/hook-payload.md`: **treat it as the source of truth and
re-verify against a real session before changing what `thirdeye mcp` assumes.**

## Process environment

The server is spawned per session, with the project directory as its cwd.

| Variable | Value seen | Why thirdeye cares |
|---|---|---|
| `CLAUDE_CODE_SESSION_ID` | `fa012d64-00f3-4ff5-9004-11da3c9e27ac` | **Load-bearing.** Flags go to this session's directory, beside `hook.jsonl`. Without it every flag would land in `_unattributed` and never group with the hook events of the same session. |
| `CLAUDE_PROJECT_DIR` | `/home/u/projects/app` | Workspace resolution, identical to `thirdeye hook`. |
| `CLAUDE_CODE_ENTRYPOINT` | `sdk-cli` | Reflects how `claude` was launched; unused. |
| `CLAUDECODE` | `1` | Unused. |
| `CLAUDE_PID` | pid of the `claude` process | Unused. |

Both `CLAUDE_CODE_SESSION_ID` and `CLAUDE_PROJECT_DIR` were present at process
startup *and* still present at `tools/call` time — checked separately, because
reading them once at startup is only safe if they do not change.

The session id here is the same shape as the hook payload's `session_id`
(a UUID). **That they are the same *value* for one session is what makes flags
group with hook events**, and it is verified by the end-to-end check in
`docs/install.md`, not by this capture.

## `initialize`

The client sends:

```jsonc
{
  "protocolVersion": "2025-11-25",
  "capabilities": { "roots": { "listChanged": true }, "elicitation": {} },
  "clientInfo": {
    "name": "claude-code",
    "title": "Claude Code",
    "version": "2.1.220"
  }
}
```

`thirdeye mcp` echoes the requested `protocolVersion` back rather than
asserting one of its own — its whole surface is `initialize`, `tools/list` and
`tools/call`, none of which have changed across revisions.

## `tools/call`

```jsonc
{
  "name": "flag",
  "arguments": { "path": "src/log.rs", "reason": "…" },
  "_meta": {
    "claudecode/toolUseId": "toolu_014hof3GmM81ELHEzKfnB9t4",
    "progressToken": 2
  }
}
```

Two things follow:

- **`_meta["claudecode/toolUseId"]` is the same `toolu_…` shape as the hook
  payload's `tool_use_id`**, so a flag carries a real `dedup_key` and a future
  transcript tailer reporting the same call dedups exactly rather than by
  timing.
- **There is no `prompt_id`.** Nothing in the call says which turn is
  underway. `thirdeye mcp` therefore reads the session log back and takes the
  open turn (`open_turn_id`) — the reason that function exists.

## Transport

Newline-delimited JSON over stdio, one message per line.

**stdout belongs to JSON-RPC.** Anything else written there desynchronizes the
client. This is the exact inverse of the hook path, where *stderr* is the
forbidden channel because a hook's stderr surfaces inside the agent session.

Notifications (`notifications/initialized`) carry no `id` and must draw no
reply.
