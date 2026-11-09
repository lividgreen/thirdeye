# pi Adapter Implementation Plan

**Goal:** thirdeye works with pi — turns, reads, writes, edits (with range),
shell writes, flags, and the viewer in tmux, WezTerm or kitty.

**Architecture:** the OpenCode shape, reused: an embedded extension
(`agents/pi/extension.js`) forwards raw events in the envelope `{ hook,
session, turn, cwd, event }` to `thirdeye hook --agent pi`; `pi.rs`
translates. The extension mints the turn id pi lacks. A `thirdeye_flag` tool
runs `thirdeye flag` with the exact session. Facts:
`docs/pi-extension-events.md`. Unlike OpenCode, the extension runs in the
agent's process, so surfaces need nothing special.

**Constraints:** as for OpenCode — raw events only interpreted in `pi.rs`;
`content`/`structuredContent` never stored; `bash` bracketed; `read`/`write`/
`edit` translated; the extension never throws into pi and never holds it up
for longer than a short timeout.

## Task 1 — adapter
- Move `edit_range`/`hunk_span` from `opencode.rs` into a shared
  `thirdeye-adapters/src/patch.rs` (`pub fn span(patches: impl Iterator<Item = &str>) -> Option<LineRange>`); OpenCode tests unchanged.
- `AgentId::Pi` (serialized `"pi"`), `AgentArg::Pi`, `pi.rs` with tests:
  prompt opens a turn; `turn.end` closes it; read/write/edit recorded with
  paths and turn; edit range from `details.patch`; bash bracketed;
  `isError` file tools not recorded; contents never logged; `session.end`
  ends the session; a tool with no turn still recorded.

## Task 2 — extension and installer
- `agents/pi/extension.js`, `agents/pi/skills/flagging-files/SKILL.md`.
- Generalize `cmd_opencode.rs`'s install/uninstall into
  `crates/thirdeye/src/agent_files.rs` (one plugin file, one owned skill,
  empty-parent cleanup); `cmd_opencode` and new `cmd_pi` call it. `thirdeye
  pi install --project <dir>` → `<dir>/.pi/{extensions/thirdeye/index.js,
  skills/flagging-files/SKILL.md}`; `--user` → `~/.pi/agent/…`. Absolute
  binary path, as for OpenCode (pi's own PATH is the terminal's, but the
  rule is kept uniform for plugin-style agents).
- Node-driven tests of the extension against a stand-in pi API: a hung
  thirdeye never holds pi up; dash-leading flag words survive; a flag without
  a session is refused; an unserializable event does not throw.
- xtask `Pi` agent supported.

## Task 3 — live verification and docs
- `scripts/e2e-pi.sh` (`pi -p --model "${THIRDEYE_PI_MODEL:-openai-codex/gpt-6-luna}" --no-extensions -e <installed extension>`), kitty split; a tmux pane check.
- `docs/install.md` pi section; CLAUDE.md seam table and Built paragraph.
