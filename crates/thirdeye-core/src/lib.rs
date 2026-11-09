//! Core of **thirdeye** — a companion tool that watches a CLI coding agent
//! and shows you which files are currently in play.
//!
//! # Shape
//!
//! Producers (agent hooks, an MCP server, transcript tailers, state-DB
//! pollers) and consumers (the TUI, later a GUI or web head) never talk to
//! each other. They meet at an append-only, session-keyed event log:
//!
//! ```text
//! adapters ──► FileEvent ──► log segments ──► TrackerState ──► heads
//! ```
//!
//! Two rules keep that seam intact, and everything else follows from them:
//!
//! 1. **Adapters translate; heads consume.** No head parses an agent-specific
//!    format. Adding a GUI must not require touching adapter code, and adding
//!    an agent must not require touching head code.
//! 2. **`TrackerState` is per-session.** How a head decides *which* session it
//!    is attached to is a head concern, kept out of the core entirely.

pub mod event;
pub mod location;
pub mod log;
pub mod tracker;
pub mod turns;

pub use event::{
    AgentId, EventKind, FileEvent, LOG_FORMAT_VERSION, LineRange, LogRecord, Source, Timestamp,
    TurnKind, TurnMarker,
};
pub use location::{EventPaths, SUN_PATH_MAX, UNATTRIBUTED, Workspace, home_dir};
pub use log::{LogError, ReadOutcome, Writer, read_session};
pub use tracker::{KindCounts, Staleness, TrackedFile, TrackerState};
pub use turns::{Turn, group_into_turns, open_turn_id};

/// Loads a session's log and folds it into a tracker state.
///
/// The common path for every head: read what has accumulated, then keep
/// applying events as they arrive.
pub fn load_session(
    paths: &EventPaths,
    session: Option<&str>,
) -> Result<(TrackerState, ReadOutcome), LogError> {
    let outcome = read_session(paths, session)?;
    let mut state = TrackerState::new(paths.workspace().clone(), session.map(str::to_owned));
    state.apply_all(outcome.file_events());
    Ok((state, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn end_to_end_write_then_rank() {
        let root = std::env::temp_dir().join(format!("thirdeye-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = EventPaths::with_state_root(&root, Workspace::new("/home/u/app"));

        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        w.append(&LogRecord::File(
            FileEvent::new(
                Timestamp(1_000),
                AgentId::Claude,
                "/home/u/app/src/lib.rs",
                EventKind::Read {
                    range: Some(LineRange::new(1, 50)),
                },
                Source::Hook,
            )
            .with_session("s1")
            .with_dedup_key("toolu_1"),
        ))
        .unwrap();
        w.append(&LogRecord::File(
            FileEvent::new(
                Timestamp(2_000),
                AgentId::Claude,
                "/home/u/app/src/main.rs",
                EventKind::Edit {
                    range: Some(LineRange::new(10, 12)),
                },
                Source::Hook,
            )
            .with_session("s1")
            .with_dedup_key("toolu_2"),
        ))
        .unwrap();

        let (state, outcome) = load_session(&paths, Some("s1")).unwrap();
        assert_eq!(outcome.malformed_lines, 0);
        assert_eq!(state.len(), 2);

        let ranked = state.ranked(Timestamp(2_000));
        assert_eq!(state.display_path(ranked[0]), Path::new("src/main.rs"));
        assert_eq!(ranked[0].last_range, Some(LineRange::new(10, 12)));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn sessions_are_isolated_within_one_workspace() {
        // Two agents in the same project must not bleed into each other's
        // view — this is what a merely-cosmetic session key would fail.
        let root: PathBuf =
            std::env::temp_dir().join(format!("thirdeye-iso-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = EventPaths::with_state_root(&root, Workspace::new("/home/u/app"));

        for (session, file) in [("s1", "/a.rs"), ("s2", "/b.rs")] {
            Writer::open(&paths, Some(session), "hook")
                .unwrap()
                .append(&LogRecord::File(
                    FileEvent::new(
                        Timestamp(1),
                        AgentId::Claude,
                        file,
                        EventKind::Write,
                        Source::Hook,
                    )
                    .with_session(session),
                ))
                .unwrap();
        }

        let (s1, _) = load_session(&paths, Some("s1")).unwrap();
        let (s2, _) = load_session(&paths, Some("s2")).unwrap();

        assert_eq!(s1.len(), 1);
        assert_eq!(s2.len(), 1);
        assert!(s1.get(Path::new("/a.rs")).is_some());
        assert!(s1.get(Path::new("/b.rs")).is_none());

        let mut found = paths.sessions().unwrap();
        found.sort();
        assert_eq!(found, vec!["s1".to_string(), "s2".to_string()]);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
