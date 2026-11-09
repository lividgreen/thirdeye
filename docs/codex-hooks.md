# Codex hooks — what the sources say, before the capture

**Status: superseded by the capture — read `docs/codex-hook-payload.md`
first.** Kept for what the docs claimed and where the capture disagreed (above
all: no `apply_patch` was ever seen; Codex edited through the shell).

**Original status: unverified.** Everything here comes from Codex's published docs and
the JSON schemas generated from its source, read on 2026-10-08. The project
rule is that payloads are *captured*, never inferred (`docs/hook-payload.md`
is the Claude Code precedent), so no adapter code may rely on this page.
`scripts/codex-capture.sh` produces the capture; its findings replace this
page as `docs/codex-hook-payload.md`.

Sources:

- Hooks guide: <https://learn.chatgpt.com/docs/hooks> (redirected from
  `developers.openai.com/codex/hooks`).
- Input schemas, one per event:
  `codex-rs/hooks/schema/generated/<event>.command.input.schema.json` in
  <https://github.com/openai/codex>, generated from `codex-rs/hooks/src/schema.rs`
  with `deny_unknown_fields`.

## Why this looks cheap

Codex's hooks are close to a copy of Claude Code's:

| | Claude Code | Codex (per docs) |
|---|---|---|
| Config | `hooks/hooks.json` in a plugin | the same file, same shape; also `[hooks]` in `config.toml` |
| Plugin root variable | `CLAUDE_PLUGIN_ROOT` | `PLUGIN_ROOT`, **and** `CLAUDE_PLUGIN_ROOT` |
| Payload | one JSON object on stdin | the same |
| Events thirdeye uses | `PostToolUse`, `UserPromptSubmit`, `Stop`, `SessionEnd` | all four exist |
| Background hooks | `"async": true` | `"async": true`, at most 8 at once per session |
| Matcher | regex on tool name | regex on tool name |

So `agents/codex/` may be little more than today's `agents/claude/` with a
different manifest. The adapter is where the real work is.

## What differs

- **The turn id is `turn_id`, not `prompt_id`.** It is on `UserPromptSubmit`,
  `PostToolUse` and `Stop` — required in every one of their schemas — so
  turn grouping should carry over by renaming one field.
- **Edits arrive as `apply_patch`**, with the patch text in
  `tool_input.command`. Matchers `Edit` and `Write` also match it. The adapter
  must parse the patch for paths and line ranges — Codex's
  `*** Update File:` / `*** Add File:` / `*** Delete File:` / `*** Move to:`
  format, to be confirmed by the capture.
- **Reads arrive only as shell commands** (`tool_name` `Bash`, the command in
  `tool_input.command`). There is no `Read` tool. Reads are the lowest-weight
  event in ranking, so an adapter that misses some degrades gracefully; one
  that guesses wrongly about `sed -n 10,20p` puts files in the list that were
  never read. Parse a small, certain subset or record none — decide against
  real captured commands, not imagined ones.
- **`tool_input` and `tool_response` are untyped** in the schemas (`true`).
  Only the capture can say what they hold.
- **`SessionEnd` always runs synchronously, with a 1-second default timeout
  (3 at most).** thirdeye's `SessionEnd` path runs `systemctl --user stop`,
  which waits for the unit. If that can exceed a second, Codex kills the hook
  and the daemon outlives the session.
- **`Stop` "expects JSON on stdout when it exits 0".** thirdeye's hook prints
  nothing. Whether Codex treats empty stdout as invalid — a warning in the
  user's session, or worse — is a capture question.
- **Hooks need trust.** Non-managed hooks, plugin hooks included, are skipped
  until the user approves them in `/hooks`, and trust is keyed on a hash of
  the hook definition — so **every change to the plugin's `hooks.json` needs
  re-approval**. The install docs must say so, as they already say Claude Code
  needs a restart.
- **`SessionEnd` carries `reason`, and it is always `other`** for now.

## Questions the capture must answer

1. The exact `tool_input` / `tool_response` for `apply_patch` and for `Bash`.
   Does `tool_response` embed file content, as Claude's does? (The log must
   never hold it.)
2. Is `tool_use_id` on `PostToolUse` the same id a transcript tailer would
   see? It is what dedup keys on.
3. Does `Stop` with empty stdout produce a warning?
4. How long does a `SessionEnd` hook have in practice, and does `codex exec`
   fire it at all?
5. **Does an MCP server get the session id?** thirdeye's `flag` tool reads
   `CLAUDE_CODE_SESSION_ID`; without an equivalent every Codex flag lands in
   `_unattributed`. The capture dumps an MCP server's environment to find out.
6. Do hooks see the agent's terminal environment (`TMUX`, `KITTY_LISTEN_ON`)?
   Surfaces depend on it.
7. Does `exec_command`'s `write_stdin` polling deliver one `PostToolUse` per
   command, or several?

## Running the capture

Needs Codex installed and signed in (`npm i -g @openai/codex`, then
`codex login`). It makes one `codex exec` call, which costs whatever your
OpenAI plan charges for a short task.

    scripts/codex-capture.sh

It runs in a throwaway `CODEX_HOME` with your `auth.json` linked in, so your
own Codex config, hooks and trust records are never touched, and it passes
`--dangerously-bypass-hook-trust` because `codex exec` cannot show the trust
prompt. Raw captures stay in `~/.cache/thirdeye-codex-capture-*` — they
contain file contents, and are not committed, exactly as for Claude Code. Not
`/tmp`: Codex refuses to install its helper binaries under a temporary
directory, and the capture should see what a real install does.

Verified flag spellings against codex-cli 0.161.0, which removed
`--full-auto`; `--sandbox workspace-write` replaces it.
