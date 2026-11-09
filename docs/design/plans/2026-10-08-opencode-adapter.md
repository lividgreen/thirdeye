# OpenCode Adapter Implementation Plan

**Goal:** thirdeye works with OpenCode 2 — turns, the files it read, wrote
and edited (with edit ranges), its shell writes, and flags.

**Architecture:** OpenCode has no shell hooks; it loads JS plugins into its
server. thirdeye ships one plugin, `agents/opencode/plugin.js`, embedded in the
binary like the systemd units. It forwards each event it sees — wrapped in a
small envelope that adds what OpenCode does not put on tool events (the turn,
the directory) — to `thirdeye hook --agent opencode`, where `opencode.rs`
translates it. The plugin is **glue, not an adapter**: it forwards raw events
and never interprets them, so the rule "adapters are the only place an
agent-specific format is read" holds. Flags need no MCP server: the plugin
defines a `thirdeye_flag` tool, whose `execute` receives the exact session id,
and runs a new `thirdeye flag` command.

**Tech Stack:** Rust 2024, `serde_json`; plain ES-module JavaScript for the
plugin (Node/Bun built-ins only). No new crates.

**Spec:** none for OpenCode (plans do not edit `docs/design/specs/`).
The plan argues from the captured facts in `docs/opencode-plugin-events.md`.

## Global Constraints

- Payload facts come only from `docs/opencode-plugin-events.md`.
- `result` (file contents, command output) is never stored. The plugin
  forwards it — as Claude Code's hook forwards `tool_response` — and the
  adapter reads only `edit`'s patch hunk headers.
- `shell` is bracketed (`Observe`), as for Claude's `Bash` and every Codex
  call. `read`/`write`/`edit` are translated. Code Mode's `execute` wrapper is
  ignored; its nested tools report themselves.
- `thirdeye hook` with no `--agent`, and every existing adapter, behave
  exactly as before.
- The plugin must never break OpenCode: every spawn is caught, never rejects,
  and writes nothing to OpenCode's stdout.
- Every task ends green on the four gates; new indicators get perturbation
  tests.

## Review Focus

1. **The shared background service** — the plugin sees the service's
   environment, not the terminal's, so the viewer cannot pick a surface on its
   own. Documented as a known gap; the viewer is still one `thirdeye head`
   away (Task 4 doc).
2. **A tool event before any prompt** (thirdeye installed mid-session) — no
   turn in the envelope; events must still be recorded, ungrouped (Task 2).
3. **A failed `edit`/`write`** (`status` not `completed`) — the file did not
   change; it must not be recorded as changed (Task 2).
4. **A server serving several sessions** — turns are tracked per session; one
   session's prompt must not become another's turn (Task 3, plugin).
5. **`thirdeye` not on the server's `PATH`** — the plugin names the binary as
   the installer resolved it, exactly as the Codex config does (Task 3).

---

## Task 1: `thirdeye flag`

**Files:** `crates/thirdeye/src/cmd_mcp.rs`, new `crates/thirdeye/src/cmd_flag.rs`, `main.rs`

**Produces:**
- `cmd_mcp::record_flag(paths: &EventPaths, workspace: &Workspace, agent: AgentId, session: Option<String>, path: &str, reason: &str, range: Option<LineRange>, dedup: Option<&str>, now: Timestamp, notify: impl Fn(&Path)) -> Result<String>` — today's flag body, moved out of `Server::call_tool` (which calls it); returns the confirmation text.
- `thirdeye flag --agent <a> --session <s> --path <p> --reason <r> [--start-line N --end-line M]`, workspace from the current directory; prints the confirmation on stdout.

- [ ] Tests (`cmd_flag.rs`): `a_flag_from_the_command_line_lands_in_its_session` (session dir holds one Flag with the agent and reason); `a_range_is_carried`.
- [ ] Red → move the body, add the command → green; existing `cmd_mcp` tests unchanged.
- [ ] Commit: `feat(flag): a command-line flag, for agents without MCP`

## Task 2: The OpenCode adapter

**Files:** new `crates/thirdeye-adapters/src/opencode.rs`, `lib.rs`, `main.rs` (`AgentArg::Opencode`)

The envelope (written by the plugin, Task 3):

```jsonc
{ "hook": "prompt" | "tool.before" | "tool.after" | "turn.end" | "session.end",
  "session": "ses_…", "turn": "msg_…" /* may be absent */, "cwd": "/abs/project",
  "event": { /* the raw OpenCode object */ } }
```

Mapping: `prompt` → turn open (`turn`, `event.prompt.text`); `turn.end` →
turn close; `session.end` → `Lifecycle::End`; `tool.before` + `shell` →
`Observe::Begin { call: event.id }`; `tool.after` + `shell` → `Observe::End
{ call, turn }`; `tool.after` with `status == "completed"`: `read` → `Read
{ range: None }`, `write` → `Write`, `edit` → `Edit { range }` with the range
spanning every `@@ -a,b +c,d @@` hunk in `event.result.output.files[].patch`
(new-file coordinates, as Claude's `edit_range`); paths from
`event.input.path`, absolutized against `cwd`; `dedup_key` = `event.id`.
Anything else → empty payload.

- [ ] Tests, built from `docs/opencode-plugin-events.md`: `a_prompt_opens_a_turn`; `an_execution_ending_closes_the_turn`; `a_read_write_and_edit_are_recorded_with_their_paths`; `an_edit_carries_its_hunk_range`; `a_shell_call_is_bracketed`; `a_failed_edit_is_not_recorded` (Review Focus 3); `a_tool_with_no_turn_is_still_recorded` (Review Focus 2); `file_contents_never_reach_the_log`; `code_mode_wrappers_are_ignored`.
- [ ] Red → implement → green; route `AgentId::Generic("opencode")`? No — add `AgentId::OpenCode` to core only if it serializes compatibly; otherwise **ruling** in the ledger.
- [ ] Commit: `feat(adapters): opencode`

## Task 3: The plugin, and `thirdeye opencode install`

**Files:** new `agents/opencode/plugin.js`, `agents/opencode/skills/flagging-files/SKILL.md`, new `crates/thirdeye/src/cmd_opencode.rs`, `main.rs`, `xtask/src/agent.rs`; move `invocation`/`thirdeye_on_path` from `cmd_codex.rs` to a shared `crates/thirdeye/src/invocation.rs`.

Plugin (`{{THIRDEYE}}` → a JSON string literal of the binary): keeps
`turns: Map<sessionID, messageID>` and a `Set` of sessions; `prompt` hook sets
the turn and forwards; `tool.execute.before` forwards **and awaits** only for
`shell`; `execute.after` forwards (not awaited); event subscription forwards
`session.execution.succeeded|failed` as `turn.end` and, on
`location.shutdown`, one `session.end` per session seen; defines
`thirdeye_flag` (`path`, `reason`, optional `start_line`/`end_line`) whose
`execute` runs `thirdeye flag --agent opencode --session <context.sessionID>
…` in `ctx.location.directory` and returns its stdout. Every spawn: stdio
`["pipe","pipe","ignore"]`, errors swallowed.

Install: `--project <dir>` → `<dir>/.opencode/{plugins/thirdeye/index.js,
skills/flagging-files/SKILL.md}`; `--user` → `~/.config/opencode/…`. Only our
own files — no config merge. Uninstall removes them and parents left empty.

- [ ] Tests (`cmd_opencode.rs`): `install_writes_the_plugin_and_skill`; `the_plugin_runs_this_binary` (rendered literal is valid JSON for an odd path); `uninstall_leaves_nothing_where_there_was_nothing`; `the_users_own_plugins_survive`; template still holds the placeholder.
- [ ] Plugin syntax: `node --check` on the rendered file in a test-time step (skip if `node` absent).
- [ ] xtask `OpenCode` agent supported; `claude_codex_and_opencode_are_supported`.
- [ ] Commit: `feat(opencode): the plugin, and thirdeye opencode install`

## Task 4: Live verification and docs

- [ ] `scripts/e2e-opencode.sh` — scratch project, `thirdeye opencode install --project`, one `opencode run --standalone --auto -m "${THIRDEYE_OPENCODE_MODEL:-opencode/mimo-v2.6-flash-free}"` that reads, writes, edits, runs a writing shell command and flags; assert turn open/close, read/write/edit/shell-write events, edit range, flag in its session labelled `opencode`, no `alpha` in the log, line cap; kitty split when a kitty is launched for it (standalone inherits the env).
- [ ] tmux: run it inside a private tmux server; the pane must open.
- [ ] Docs: `docs/install.md` OpenCode section (incl. the service-environment gap), CLAUDE.md (seam table, Built paragraph, gotcha).
- [ ] Commit: `test(e2e): opencode, end to end`
