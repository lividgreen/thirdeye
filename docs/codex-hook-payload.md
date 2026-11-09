# Codex hook payloads (verified)

Captured on 2026-10-08 from codex-cli 0.161.0 (model `gpt-6-luna`) by
`scripts/codex-capture.sh`: two headless `codex exec` runs with a hook that
dumped stdin and environment for every event. Raw captures are **not**
committed (they contain file contents). `docs/codex-hooks.md` holds what the
docs claimed beforehand; this page is what actually arrived.

## The finding that shapes the adapter

**Codex did every file change through the shell.** Across both runs, with a
prompt naming shell commands and with a neutral one, there was not one
`apply_patch` call:

| Run | Prompt | How Codex changed files |
|---|---|---|
| 1 | named `cat` and `sed -n`, then asked for an edit | `printf 'checked\n' > notes.md && sed -i 's/beta/BETA/' data.txt` |
| 2 | neutral: "create notes.md… change 'beta' to 'BETA'" | one `Bash` call running a Python heredoc that rewrote both files |

The payload carries only the command text. Nothing structured anywhere says
which files changed — and the transcript (`$CODEX_HOME/sessions/…/rollout-
*.jsonl`) has no diff events either, only the same command. **An adapter that
reads the hook payload alone cannot know what Codex mutated**, short of
interpreting arbitrary programs. `apply_patch` may still appear with other
models or prompts; two runs do not prove it never does.

## SessionStart

```jsonc
{
  "session_id":      "01a11a81-091b-7df3-9639-8ec14a21a543",
  "transcript_path": "…/sessions/2026/10/08/rollout-….jsonl",
  "cwd":             "/abs/project",
  "hook_event_name": "SessionStart",
  "model":           "gpt-6-luna",
  "permission_mode": "bypassPermissions",
  "source":          "startup"
}
```

## UserPromptSubmit

```jsonc
{
  "session_id": "…", "transcript_path": "…", "cwd": "…",
  "turn_id":         "01a11a81-0ee1-7c92-96d0-a404f92fe3ec",
  "hook_event_name": "UserPromptSubmit",
  "model": "…", "permission_mode": "…",
  "prompt":          "Run `cat data.txt`. Then …"
}
```

`turn_id` is Claude Code's `prompt_id`, under another name: the same value on
`UserPromptSubmit`, every `PostToolUse` of the turn, and `Stop`.

## PostToolUse (Bash)

```jsonc
{
  "session_id": "…", "transcript_path": "…", "cwd": "…",
  "turn_id":         "01a11a81-0ee1-7c92-96d0-a404f92fe3ec",
  "hook_event_name": "PostToolUse",
  "model": "…", "permission_mode": "…",
  "tool_name":       "Bash",
  "tool_input":      { "command": "cat data.txt" },
  "tool_response":   "alpha\nbeta\ngamma\n",
  "tool_use_id":     "exec-ccc494e4-0384-49a9-a609-6f9ab783c7b8"
}
```

- **`tool_response` is the command's output, as a plain string — which for
  `cat` is the file's content.** Discard it, exactly as for Claude Code.
- `tool_input` is `{ "command": <string> }`; the command can be a multi-line
  heredoc.
- **`tool_use_id` is the transcript's `CommandExecution` item id** (the `id`
  of the `item` in an `item_completed` event), *not* the model-level
  `call_id` of the `custom_tool_call`. A transcript tailer keyed on the item
  id dedups exactly against the hook.

## Stop

```jsonc
{
  "session_id": "…", "transcript_path": "…", "cwd": "…",
  "turn_id":                "01a11a81-0ee1-7c92-96d0-a404f92fe3ec",
  "hook_event_name":        "Stop",
  "model": "…", "permission_mode": "…",
  "stop_hook_active":       false,
  "last_assistant_message": "DONE"
}
```

The docs say `Stop` "expects JSON on stdout when it exits 0". A hook that
printed **nothing** produced no warning — Codex logged `hook: Stop Completed`.
thirdeye's hook can stay silent.

## SessionEnd

```jsonc
{
  "session_id": "…", "transcript_path": "…", "cwd": "…",
  "hook_event_name": "SessionEnd",
  "reason":          "other"
}
```

Fires under `codex exec`, about 20 ms after `Stop`. It runs synchronously with
a 1-second default timeout (`docs/codex-hooks.md`); how long thirdeye's
`systemctl --user stop` takes inside that budget was not measured.

## Environment

- **A hook inherits the agent's environment**, and Codex adds only
  `CODEX_HOME` to it. `TMUX`, `TMUX_PANE` and `KITTY_LISTEN_ON` will reach the
  hook as they do under Claude Code, so surfaces need no Codex-specific work.
  No session id is exported — it is in the payload only.
- **An MCP server gets a scrubbed environment**: none of the agent's
  variables, and nothing added — no session id, no turn id. thirdeye's `flag`
  tool finds its session in `CLAUDE_CODE_SESSION_ID`; under Codex it would
  file every flag as `_unattributed`.
- The first capture ran from inside a Claude Code session and saw
  `CLAUDE_CODE_SESSION_ID` in Codex's hooks — inherited from Claude, not set
  by Codex. The script now strips `CLAUDE*` before running Codex; anyone
  repeating a capture from inside an agent should check the same.

## Not yet seen

- An `apply_patch` payload, or any MCP tool's `PostToolUse`.
- `exec_command` with `write_stdin` polling (whether one command yields one
  `PostToolUse`).
- A resumed session's `SessionStart` (`source: "resume"`).
