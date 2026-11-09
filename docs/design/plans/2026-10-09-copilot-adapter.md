# Copilot CLI Adapter Implementation Plan

**Goal:** thirdeye works with GitHub Copilot CLI: turns, reads, edits,
writes, searches, shell writes, flags, and the viewer in tmux, WezTerm or
kitty.

**Facts:** `docs/copilot-hook-payload.md`, captured from 1.0.94. Three of
them shape everything below:

1. **No payload names its event.** The hook command says which event it is
   registered for: `thirdeye hook --agent copilot --event postToolUse`. The
   hook wraps the payload in the envelope OpenCode and pi already use,
   `{ "hook": <event>, "event": <payload> }`, so `copilot.rs` reads a shape
   of the same family.
2. **No turn id.** Copilot's hooks are synchronous and its events arrive in
   order, so the hook keeps the turn itself. The adapter marks the edges:
   `Payload::turn` = `Open(id)` (the id minted from the prompt's
   `timestamp`), `Within`, or `Close`. The hook persists the open turn
   beside the session's observe markers, stamps it on records and brackets
   that carry none, and writes the close marker from it. Like the markers, it
   is hook-side scratch: the log still ends up holding every id, so the
   daemon still owns nothing. `SessionEnd` clears it.
3. **No tool-call id.** A bracket is keyed by a hash of `toolName` +
   `toolArgs` (FNV-1a, never `DefaultHasher`). Pre and post are sequential,
   so only two identical calls running in parallel could collide. Such a key
   is not unique per call, so `Observe::End` says whether `call` is the
   agent's own id. Observed writes under a derived key get no `dedup_key`:
   running `cargo fmt` every turn must count every time. Deletions keep their
   turn-keyed key.

**Native events, not the PascalCase variant.** That variant renames the
tools to Claude's but keeps Copilot's arguments (`Read` with `path`, `Edit`
with a patch string), so it is not Claude's shape either. The native names
are Copilot's real ones.

**Tools.** Copilot's tool set depends on the model, and only one model's
could be captured. So:
- `view` → `Read` (no range). `rg`, `grep`, `glob` → `Search`; a search
  without `path` covers the session's directory.
- `apply_patch` → parsed: `*** Update File: p` → `Edit` (no range: hunks
  are context-anchored), `*** Add File: p` → `Write`, `*** Delete File: p`
  → `Delete`, `*** Move to: q` → `Write` of `q` and `Delete` of the source.
  Paths are relative to `cwd`.
- `bash` → bracketed, as every agent's shell.
- **Any other tool → bracketed.** A model that edits through `edit` or
  `create` is still seen, as writes without a range, instead of not at all.
  The cost is a workspace walk per such call. Only the tools translated
  above are exempt; MCP tools (`<server>-<tool>`, verified) are bracketed
  too, which costs a walk and nothing else.
- A `postToolUseFailure` records nothing but still closes a bracket.
- `toolResult` is never read.

**Guard (roadmap 0.3).** `COPILOT_PROJECT_DIR` joins
`CLAUDE_HOOK_BORROWERS`. Hooks get it; Copilot's shell tool does not.

## Task 1 — adapter and hook

- `AgentId::Copilot` (`"copilot"`), `AgentArg::Copilot`, `thirdeye hook
  --event <name>` (only meaningful with an envelope-less agent; the hook
  wraps when given).
- `Payload::turn: Option<TurnEdge>` and the hook's turn file; `Observe::End`
  gains whether the call id is the agent's.
- `crates/thirdeye-adapters/src/copilot.rs`, tests from the captured
  payloads: the prompt opens a turn; `agentStop` closes it; `view` is a
  read; `apply_patch` update/add/delete/move; `rg`/`glob` searches; `bash`
  bracketed under a derived key, pre and post agreeing; an unknown tool
  bracketed; a failed `view` records nothing; `textResultForLlm` never
  reaches the log; `sessionEnd` ends the session.
- Hook tests: a turn opened by one invocation is stamped on the next and
  closed by a third; a derived-key write is recorded twice across two calls
  to the same command.
- The guard, with its test.

## Task 2 — plugin and installer

**Verify first** (cheap, no model call needed for most):
- A plugin in a *local marketplace* loads live from its directory (`copilot
  plugin marketplace add <dir>`, `copilot plugin install thirdeye@thirdeye`),
  rather than being copied as a direct install is. A copy would bring back
  the stale-plugin trap Claude's cache has.
- `.mcp.json` in a plugin: its key name, and whether the MCP server learns
  the session. If not, `flag` falls back to `active-session`, as for Codex.
- The MCP tool's name as a hook sees it.

Then:
- `agents/copilot/` — `plugin.json`, `hooks.json` (the binary's absolute
  path, as for OpenCode; each event registered with its `--event`),
  `.mcp.json`, `skills/flagging-files/SKILL.md`, and the marketplace
  manifest.
- `thirdeye copilot install --user | uninstall`: writes the marketplace
  under `$XDG_DATA_HOME/thirdeye/copilot/` and registers it through the
  `copilot` CLI. **`--project` is refused with the reason:** Copilot
  plugins are installed per user, and a repository can only switch one on or
  off.
- xtask `Copilot` agent; doctor `inspect()`.

## Task 3 — live verification and docs

- `scripts/e2e-copilot.sh` (`copilot -p … --allow-all --max-ai-credits
  30`), kitty split; a tmux pane check.
- `docs/install.md` Copilot section; CLAUDE.md Built paragraph, seam table,
  and a Gotcha for the no-ids facts.
- Roadmap: 0.3 done for Copilot; 1b done.
