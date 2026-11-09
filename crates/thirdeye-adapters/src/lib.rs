//! Adapters translate agent-specific transports into `LogRecord`s.
//!
//! Heads never see these formats — that separation is what lets a new agent
//! and a new head be added independently.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod grok;
pub mod opencode;
pub mod patch;
pub mod pi;
pub mod shell;

use std::path::{Path, PathBuf};

use thirdeye_core::{AgentId, EventKind, LogRecord, Timestamp};

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("could not parse hook payload: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("thirdeye has no adapter for {0}")]
    NoAdapter(String),
}

/// What one hook invocation yielded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Payload {
    pub session: Option<String>,
    pub cwd: Option<PathBuf>,
    pub records: Vec<LogRecord>,
    /// Session lifecycle, if this payload marks one.
    ///
    /// Not a `LogRecord`: the log is a record of what happened to *files*, and
    /// a session ending touches none. It is normalized here rather than left
    /// as a string so that acting on it does not mean a caller reading an
    /// agent-specific field — the rule the whole adapter seam exists for.
    pub lifecycle: Option<Lifecycle>,
    /// A tool call starting or finishing, for the hook to watch the
    /// workspace across.
    ///
    /// For tools whose payload cannot say what they changed — anything that
    /// runs an arbitrary command. The adapter only marks the bracket; finding
    /// what was written is the hook's, because it is the same filesystem
    /// question whichever agent asked it.
    pub observe: Option<Observe>,
    /// For an agent whose events name no turn: which edge of one this is.
    /// The hook keeps the open turn between invocations; see [`TurnEdge`].
    pub turn: Option<TurnEdge>,
    /// Whether `observe`'s call id was derived from the call rather than
    /// given by the agent. A derived id is shared by every run of the same
    /// command, so it cannot key what one run wrote.
    pub call_ids_derived: bool,
}

/// A turn as an agent without turn ids reports it.
///
/// Such an agent says when a prompt arrives and when the agent stops, and
/// nothing in between names the turn. Its hooks are synchronous, so the
/// events arrive in order and the turn open at any moment is well defined:
/// the hook remembers it rather than the adapter guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEdge {
    /// A turn opened, with this id; the adapter has written its marker.
    Open(String),
    /// Within whatever turn is open: records without a turn join it.
    Within,
    /// The open turn ended; the hook writes the close marker.
    Close,
}

/// A session boundary, as any agent would express one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Start,
    End,
}

/// Whether `path` is inside a skills directory one of the agents loads from:
/// `.codex/`, `.agents/`, `.opencode/`, `.pi/`, `.grok/` and `.claude/` (in a project
/// or the home directory), `~/.config/opencode/`, `~/.pi/agent/` and
/// `~/.gemini/antigravity-cli/`. A
/// project's own `skills/` folder is just code.
pub fn is_skill_file(path: &Path) -> bool {
    let parts: Vec<&std::ffi::OsStr> = path.iter().collect();
    parts.windows(2).enumerate().any(|(i, pair)| {
        if pair[1] != "skills" {
            return false;
        }
        let parent = pair[0].to_string_lossy();
        let grandparent = i.checked_sub(1).map(|g| parts[g].to_string_lossy());
        match parent.as_ref() {
            ".codex" | ".agents" | ".opencode" | ".pi" | ".claude" | ".grok" => true,
            "opencode" => grandparent.as_deref() == Some(".config"),
            "agent" => grandparent.as_deref() == Some(".pi"),
            "antigravity-cli" => grandparent.as_deref() == Some(".gemini"),
            _ => false,
        }
    })
}

/// One edge of a bracketed tool call, keyed by the agent's id for the call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observe {
    Begin {
        call: String,
    },
    /// `turn` is the agent's id for the turn in progress, so what was found
    /// is grouped with the request that caused it.
    End {
        call: String,
        turn: Option<String>,
    },
}

/// Translate one hook payload with the adapter for `agent`.
///
/// Chosen by the caller rather than sniffed from the payload: Claude Code's
/// and Codex's payloads are near copies of each other, and a shape test that
/// guessed wrong would file one agent's events as the other's without a
/// single error.
pub fn translate(agent: &AgentId, raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    let mut payload = route(agent, raw, now)?;
    // An agent loading a skill reads its SKILL.md — Codex `cat`s it, as its
    // own prompt instructs; pi reads it with `read` — and that is the agent
    // loading instructions, not working on the human's files. One rule for
    // every agent. Only reads: editing a skill is real work.
    payload.records.retain(|r| match r {
        LogRecord::File(e) => !(matches!(e.kind, EventKind::Read { .. }) && is_skill_file(&e.path)),
        _ => true,
    });
    Ok(payload)
}

fn route(agent: &AgentId, raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    match agent {
        AgentId::Claude => claude::records_from_payload(raw, now),
        AgentId::Codex => codex::records_from_payload(raw, now),
        AgentId::Copilot => copilot::records_from_payload(raw, now),
        AgentId::Antigravity => antigravity::records_from_payload(raw, now),
        AgentId::Grok => grok::records_from_payload(raw, now),
        AgentId::OpenCode => opencode::records_from_payload(raw, now),
        AgentId::Pi => pi::records_from_payload(raw, now),
        other => Err(AdapterError::NoAdapter(format!("{other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{AgentId, Timestamp};

    #[test]
    fn an_agent_without_an_adapter_is_an_error_not_a_guess() {
        // Routing an unknown agent's payload through some other agent's
        // adapter would produce plausible-looking records that are wrong.
        let err = translate(&AgentId::Cursor, "{}", Timestamp(1)).unwrap_err();
        assert!(matches!(err, AdapterError::NoAdapter(_)));
    }

    #[test]
    fn claude_payloads_translate_exactly_as_before() {
        // The dispatch must be a pure routing layer; every Claude test still
        // passes through `claude::records_from_payload` unchanged.
        let raw = r#"{"session_id":"s","cwd":"/w","prompt_id":"p","hook_event_name":"PostToolUse",
                      "tool_name":"Write","tool_input":{"file_path":"/w/a"},"tool_use_id":"t"}"#;
        assert_eq!(
            translate(&AgentId::Claude, raw, Timestamp(1)).unwrap(),
            claude::records_from_payload(raw, Timestamp(1)).unwrap()
        );
    }

    #[test]
    fn an_agent_loading_a_skill_is_not_reading_the_project() {
        // Codex loads a skill by `cat`-ing its SKILL.md — its documented
        // flow — and pi has the model read it with `read`. Neither is work
        // on the human's files, so neither belongs in their list.
        let codex = r#"{"session_id":"s1","turn_id":"t1","cwd":"/w","hook_event_name":"PostToolUse",
            "tool_name":"Bash","tool_input":{"command":"cat .codex/skills/flagging-files/SKILL.md"},
            "tool_response":"","tool_use_id":"exec-1"}"#;
        assert!(
            translate(&AgentId::Codex, codex, Timestamp(1))
                .unwrap()
                .records
                .is_empty()
        );

        let pi = r#"{"hook":"tool.after","session":"s1","turn":"t1","cwd":"/w","event":{
            "toolName":"read","toolCallId":"c1","input":{"path":"/home/u/.pi/agent/skills/x/SKILL.md"},
            "isError":false}}"#;
        assert!(
            translate(&AgentId::Pi, pi, Timestamp(1))
                .unwrap()
                .records
                .is_empty()
        );
    }

    #[test]
    fn a_skill_the_agent_edits_and_a_project_skills_folder_stay_visible() {
        // Only loading is hidden: editing a skill is real work, and a
        // project's own `src/skills/` is just code.
        let edit = r#"{"hook":"tool.after","session":"s1","turn":"t1","cwd":"/w","event":{
            "toolName":"edit","toolCallId":"c2","input":{"path":".pi/skills/x/SKILL.md"},
            "isError":false,"details":null}}"#;
        assert_eq!(
            translate(&AgentId::Pi, edit, Timestamp(1))
                .unwrap()
                .records
                .len(),
            1
        );

        let code = r#"{"session_id":"s1","turn_id":"t1","cwd":"/w","hook_event_name":"PostToolUse",
            "tool_name":"Bash","tool_input":{"command":"cat src/skills/parser.rs"},
            "tool_response":"","tool_use_id":"exec-2"}"#;
        assert_eq!(
            translate(&AgentId::Codex, code, Timestamp(1))
                .unwrap()
                .records
                .len(),
            1
        );
    }

    #[test]
    fn every_agents_skill_directories_are_recognized() {
        for path in [
            "/w/.codex/skills/a/SKILL.md",
            "/w/.agents/skills/a/SKILL.md",
            "/w/.opencode/skills/a/SKILL.md",
            "/w/.pi/skills/a/notes.md",
            "/w/.claude/skills/a/SKILL.md",
            "/home/u/.grok/skills/a/SKILL.md",
            "/home/u/.config/opencode/skills/a.md",
            "/home/u/.pi/agent/skills/a/SKILL.md",
            "/home/u/.gemini/antigravity-cli/skills/a/SKILL.md",
        ] {
            assert!(is_skill_file(std::path::Path::new(path)), "{path}");
        }
        for path in [
            "/w/src/skills/a.rs",
            "/w/agent/skills/a.md",
            "/w/skills/SKILL.md",
        ] {
            assert!(!is_skill_file(std::path::Path::new(path)), "{path}");
        }
    }
}
