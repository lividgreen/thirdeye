//! `thirdeye open`, `close` and `policy` — what the slash commands run.
//!
//! Each is one message to the daemon and then exit. They exist as CLI verbs
//! rather than as head-only keybindings because a slash command is typed at
//! the *agent*, where there is no head to press a key in — and because
//! `/thirdeye-open` has to work precisely when no viewer is on screen.

use crate::local_socket::UnixStream;
use std::io::Write;

use anyhow::{Context, Result};
use thirdeye_core::EventPaths;

use crate::daemon::protocol::{ClientMessage, HideUntil, PROTOCOL_VERSION, PolicyMode, Role};
use crate::session_ref;
use crate::workspace;

/// Send one message to a session's daemon and leave.
pub fn run(session: Option<String>, message: ClientMessage) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    let ws = workspace::resolve(None, cwd.as_deref());
    let paths = EventPaths::discover(ws);

    let env = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
    let active = session_ref::load_active(&paths.workspace_dir());
    let (session, how) = pick_session(session, env, active)?;
    if how == Picked::Active {
        eprintln!("thirdeye: session {session}, the one that acted last in this project");
    }

    let socket = paths
        .socket_path(&session)
        .context("no socket path fits for this session")?;

    // The daemon is not running when nothing has happened yet, which is a
    // perfectly ordinary state for `/thirdeye-open` to be typed in.
    let mut stream = UnixStream::connect(&socket).with_context(|| {
        format!(
            "no viewer is running for session {session}. It starts on the \
             agent's first tool call; nothing has been recorded yet."
        )
    })?;

    let hello = serde_json::to_string(&ClientMessage::Hello {
        role: Role::Head,
        protocol_version: PROTOCOL_VERSION,
        head_id: Some("cli".into()),
        launched_by_surface: false,
    })?;
    writeln!(stream, "{hello}").context("greeting the daemon")?;
    let encoded = serde_json::to_string(&message)?;
    writeln!(stream, "{encoded}").context("sending the command")?;
    stream.flush().context("flushing the command")?;

    // Windows resets a connection that is closed with something unread, and
    // the daemon has sent this "head" a snapshot; a reset can reach the
    // daemon before it has read the command. So say there is no more, and
    // stay until the daemon hangs up in turn.
    #[cfg(windows)]
    {
        let _ = stream.shutdown(std::net::Shutdown::Write);
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(1)));
        let _ = std::io::copy(&mut stream, &mut std::io::sink());
    }

    // Deliberately no wait for a reply: these are fire-and-forget commands,
    // and a slash command that blocked on a daemon would stall the agent.
    let _ = session_ref::load(&socket);
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Picked {
    Named,
    Active,
}

/// Which session a command means.
///
/// Named on the command line, or exported by Claude Code to commands typed
/// in it. Otherwise the session that acted last *in this project* — every
/// agent's hook records it — which other agents' sessions need, since none
/// exports its id to a shell. Never "whichever session exists": two projects
/// open at once would make that a guess at the wrong window.
fn pick_session(
    explicit: Option<String>,
    env: impl Fn(&str) -> Option<String>,
    active: Option<String>,
) -> Result<(String, Picked)> {
    if let Some(session) = explicit
        .or_else(|| env("CLAUDE_SESSION_ID"))
        .or_else(|| env("CLAUDE_CODE_SESSION_ID"))
    {
        return Ok((session, Picked::Named));
    }
    active.map(|s| (s, Picked::Active)).context(
        "no session given and none has acted in this project yet; pass --session \
         (`thirdeye ls` lists them)",
    )
}

/// Parse the policy names the spec's table uses — the same strings that go on
/// the wire, so a human, the CLI and the protocol all share one vocabulary.
pub fn parse_policy(raw: &str) -> Result<PolicyMode> {
    Ok(match raw {
        "any" => PolicyMode::Any,
        "attention" => PolicyMode::Attention,
        "not-this-turn" => PolicyMode::NotThisTurn,
        "never" => PolicyMode::Never,
        other => anyhow::bail!(
            "unknown policy {other:?}: expected any, attention, not-this-turn or never"
        ),
    })
}

/// Parse a hide condition, spelled the way the policy names are.
pub fn parse_hide_until(raw: &str) -> Result<HideUntil> {
    Ok(match raw {
        "next-mutation" | "mutation" => HideUntil::NextMutation,
        "next-turn" | "turn" => HideUntil::NextTurn,
        "session" => HideUntil::Session,
        other => anyhow::bail!(
            "unknown condition {other:?}: expected next-mutation, next-turn or session"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_named_or_else_the_one_active_in_this_project() {
        let none = |_: &str| None;
        let claude = |k: &str| (k == "CLAUDE_CODE_SESSION_ID").then(|| "c1".to_string());
        let active = Some("a1".to_string());

        let named = pick_session(Some("s1".into()), claude, active.clone()).unwrap();
        assert_eq!(named, ("s1".into(), Picked::Named), "the command line wins");
        let inside = pick_session(None, claude, active.clone()).unwrap();
        assert_eq!(
            inside,
            ("c1".into(), Picked::Named),
            "then Claude Code's own"
        );
        let other = pick_session(None, none, active).unwrap();
        assert_eq!(
            other,
            ("a1".into(), Picked::Active),
            "then this project's last"
        );
        assert!(pick_session(None, none, None).is_err());
    }

    #[test]
    fn the_policy_names_match_the_wire_and_the_spec() {
        // One vocabulary for the human, the CLI and the protocol. If these
        // drift, `/thirdeye-policy attention` starts meaning nothing.
        for (typed, mode) in [
            ("any", PolicyMode::Any),
            ("attention", PolicyMode::Attention),
            ("not-this-turn", PolicyMode::NotThisTurn),
            ("never", PolicyMode::Never),
        ] {
            assert_eq!(parse_policy(typed).unwrap(), mode);
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{typed}\""),
                "the CLI name and the wire name must be the same string"
            );
        }
    }

    #[test]
    fn an_unknown_policy_lists_the_real_ones() {
        // The error is the only place a human learns the vocabulary.
        let err = parse_policy("sometimes").unwrap_err().to_string();
        assert!(err.contains("not-this-turn"), "got {err}");
        assert!(err.contains("attention"), "got {err}");
    }
}
