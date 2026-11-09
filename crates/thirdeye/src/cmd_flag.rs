//! `thirdeye flag` — a flag from the command line.
//!
//! For agents whose integration already knows the session exactly and has no
//! MCP server to call: OpenCode's plugin defines its own flag tool, whose
//! `execute` receives the session id, and runs this. Same record, same log,
//! as a flag through `thirdeye mcp`.

use anyhow::Result;
use thirdeye_core::{AgentId, EventPaths, LineRange, Timestamp};

use crate::cmd_mcp::{Flag, record_flag};
use crate::workspace;

pub fn run(
    agent: AgentId,
    session: String,
    path: String,
    reason: String,
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    let paths = EventPaths::discover(workspace::resolve(None, cwd.as_deref()));
    let said = record_flag(
        &paths,
        agent,
        Some(session),
        &Flag {
            path: &path,
            reason: &reason,
            range: inclusive(start_line, end_line),
            dedup: None,
        },
        Timestamp::now(),
        crate::notify::poke,
    )?;
    println!("{said}");
    Ok(())
}

/// Lines as a person says them — "10 to 12" — as the log's end-exclusive
/// range. An end without a start means nothing and is dropped.
fn inclusive(start: Option<u32>, end: Option<u32>) -> Option<LineRange> {
    start.map(|s| LineRange::new(s, end.unwrap_or(s).saturating_add(1)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{EventKind, Workspace, read_session};

    fn paths(tag: &str) -> EventPaths {
        let base = std::env::temp_dir().join(format!("thirdeye-flag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        EventPaths::with_roots(base.join("state"), base.join("run"), Workspace::new("/w"))
    }

    fn flags(paths: &EventPaths, session: &str) -> Vec<thirdeye_core::FileEvent> {
        read_session(paths, Some(session))
            .unwrap()
            .file_events()
            .filter(|e| matches!(e.kind, EventKind::Flag { .. }))
            .cloned()
            .collect()
    }

    fn flag(p: &EventPaths, agent: AgentId, range: Option<LineRange>) -> String {
        let f = Flag {
            path: "src/a.rs",
            reason: "look here",
            range,
            dedup: None,
        };
        record_flag(p, agent, Some("s1".into()), &f, Timestamp(5), |_| {}).unwrap()
    }

    #[test]
    fn a_flag_from_the_command_line_lands_in_its_session() {
        // For agents whose own plugin knows the session exactly — OpenCode's
        // tool receives it — and so need no MCP server and no guessing.
        let p = paths("cli");
        let said = flag(&p, AgentId::Codex, None);
        assert!(said.contains("src/a.rs"), "{said}");
        let found = flags(&p, "s1");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].agent, AgentId::Codex);
        assert_eq!(found[0].path, std::path::Path::new("/w/src/a.rs"));
        assert!(matches!(&found[0].kind, EventKind::Flag { reason, .. } if reason == "look here"));
    }

    #[test]
    fn lines_are_given_inclusive_as_spoken() {
        assert_eq!(inclusive(Some(10), Some(12)), Some(LineRange::new(10, 13)));
        assert_eq!(inclusive(Some(7), None), Some(LineRange::new(7, 8)));
        assert_eq!(inclusive(None, Some(12)), None);
        let p = paths("range");
        flag(&p, AgentId::Claude, inclusive(Some(10), Some(12)));
        assert!(matches!(
            &flags(&p, "s1")[0].kind,
            EventKind::Flag { range: Some(r), .. } if *r == LineRange::new(10, 13)
        ));
    }
}
