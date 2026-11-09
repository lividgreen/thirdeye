//! What crosses the transport.
//!
//! Newline-delimited JSON, so the protocol is debuggable with `nc` and open to
//! heads that are not written in Rust — a constraint that pays off the first
//! time a web head exists.
//!
//! **The daemon sends semantic data; the head decides presentation** (§4.3).
//! Nothing here carries colour, glyphs, styled text or layout. A `★` beside a
//! flagged file is a choice `thirdeye ls` makes and a web head might make
//! differently; what crosses the wire is *that the file is flagged and why*.
//! The same rule is why paths travel absolute, with the workspace root sent
//! once in the `Snapshot`: relativizing a path for display is presentation.
//!
//! Hook and head share one socket and one grammar. That is why the first
//! message carries a role: the alternative is two socket units and two paths
//! for one session, and the socket path is already the length-constrained
//! resource (§3).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thirdeye_core::{EventKind, LineRange, Timestamp, Turn};

/// Bumped when an existing message's meaning changes.
///
/// A head and a daemon from different builds meeting is routine — the plugin
/// is installed separately from the binary — so the mismatch is reported, not
/// left to manifest as a head that waits forever for a `Snapshot`.
pub const PROTOCOL_VERSION: u32 = 1;

/// Who is connecting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// A producer saying "something changed". Carries no data: the log does.
    Hook,
    /// A viewer that wants state and will send commands back.
    Head,
}

/// Anything a client sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum ClientMessage {
    /// First message on every connection.
    Hello {
        role: Role,
        protocol_version: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        head_id: Option<String>,
        /// This head is the one a surface launched, so its leaving means the
        /// surface's window is gone — the human closed it. Any other head,
        /// a `thirdeye open` above all, may come and go freely: it
        /// disconnects right after opening a window, and reading that as a
        /// close would kill the pane it had just asked for.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        launched_by_surface: bool,
    },
    /// Move the selection. Durable: it belongs to the session, not to the head
    /// that happens to be showing it, so a restarted head resumes where the
    /// human left off.
    Select { path: PathBuf },
    /// Fold or unfold a turn. Durable for the same reason.
    ExpandTurn { turn: String, expanded: bool },
    /// Change when the viewer is allowed to appear on its own.
    SetPolicy { mode: PolicyMode },
    /// Show the viewer now, whatever the policy says.
    ///
    /// The slash commands bypass policy entirely (§10.3): a human asking for
    /// the viewer outranks a rule the same human set earlier.
    Show,
    /// Hide it now, until `until` says it may come back. Events keep being
    /// collected either way.
    Hide {
        /// Defaulted so a client from before the human could choose keeps
        /// the meaning `hide` had then.
        #[serde(default)]
        until: HideUntil,
    },
    /// Ask for a file's contents. The daemon reads the disk, not the head —
    /// which keeps heads thin, keeps git knowledge in one place, and is the
    /// difference between a remote head being possible and being a rewrite.
    RequestDetail { path: PathBuf, mode: DetailMode },
    /// The agent's session is over: close the view and stop.
    ///
    /// Where systemd runs the daemon, stopping its unit says this. Where a
    /// hook started it, nothing else can — the daemon is nobody's child by
    /// then, and a process on Windows cannot be asked to stop from outside.
    End,
}

/// Anything the daemon sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Full state, sent once on connect. A head never has to reconstruct
    /// history from patches it might have missed.
    Snapshot {
        protocol_version: u32,
        session: String,
        /// Sent once so the head can relativize paths itself.
        workspace: PathBuf,
        turns: Vec<TurnView>,
        view: ViewState,
        policy: PolicyMode,
    },
    /// Everything after the snapshot.
    ///
    /// File entries are upserted whole rather than diffed field by field:
    /// they are small, and a patch format that can express "this one field
    /// changed" is a second serialization format to keep correct.
    Patch {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        turns: Vec<TurnView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        policy: Option<PolicyMode>,
    },
    /// Answer to `RequestDetail`: raw lines and markers, never styled text.
    /// Highlighting is the head's business — a web head highlights
    /// differently than ratatui, and neither should be dictated here.
    Detail {
        path: PathBuf,
        mode: DetailMode,
        /// 1-based line number of `lines[0]`, so a head can render gutters
        /// without assuming the whole file was sent.
        first_line: u32,
        lines: Vec<String>,
        /// The range the agent actually touched, when one is known: the jump
        /// target, not a selection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        highlight: Option<LineRange>,
    },
    Close {
        reason: String,
    },
}

/// When the viewer may open itself.
///
/// Events are collected in **every** mode — this governs visibility only, and
/// never what is recorded. That separation is structural: the append happens
/// in the hook process, before any policy code runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyMode {
    /// Any trigger reopens: a mutation or a flag.
    #[default]
    Any,
    /// Only an explicit flag reopens. For when the agent edits constantly and
    /// only its deliberate signals are worth interrupting for.
    Attention,
    /// Suppressed until the next prompt. Scoped to a turn rather than a
    /// duration, because "stop popping up at me" means *about this piece of
    /// work*, and a timer would either outlive the annoyance or not reach it.
    NotThisTurn,
    /// Only the slash commands reopen it.
    Never,
}

/// What brings a hidden viewer back on its own.
///
/// Every condition is about *mutations*: a flag lifts any hide, because it is
/// the agent's one deliberate "look at this", and `/thirdeye-policy never` is
/// the way to silence that too. Chosen per hide, by the human doing the
/// hiding — the same person wants different things at different moments.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HideUntil {
    /// The next edit or write, in this turn or any other.
    NextMutation,
    /// The next edit or write in a turn after this one. What a hide meant
    /// before the human could choose, and what a window closed by hand still
    /// means, since closing it cannot say which was wanted.
    #[default]
    NextTurn,
    /// Only a flag or an explicit `thirdeye open`.
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetailMode {
    Content,
    Diff,
}

/// Durable view state — the daemon's, not the head's.
///
/// Selection and folding survive a head restart because they are decisions the
/// human made about the session. Scroll offset deliberately does *not* live
/// here: it depends on the window size, and pushing it into the daemon would
/// reduce heads to dumb framebuffers and force terminal semantics onto a
/// future web head.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<PathBuf>,
    /// Turn ids the human has folded. Absent means expanded, so a new turn
    /// arrives open without anything having to say so.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collapsed: Vec<String>,
}

/// One conversational turn, as a head needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnView {
    /// `None` for the bucket holding events no turn could be found for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub opened_at: Timestamp,
    /// A turn with no `Stop` is the one in progress.
    pub closed: bool,
    pub files: Vec<FileView>,
}

/// One file within a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileView {
    /// Absolute, and the file's identity. Display is the head's problem.
    pub path: PathBuf,
    /// What happened to it, deduplicated: `["edit", "read"]`.
    pub kinds: Vec<String>,
    /// Last range the agent touched — the jump target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<LineRange>,
    /// Set when the agent explicitly asked for the human's attention. The
    /// reason is the whole content of a flag; a flagged file without one tells
    /// the human less than not flagging it would.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flag: Option<FlagView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagView {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<LineRange>,
}

impl ClientMessage {
    /// The `head_id` of the connection that ends a session, which the
    /// daemon hears whatever its protocol version.
    pub const SESSION_END: &str = "session-end";

    /// What the end of a session sends before `End`: a head's greeting,
    /// because only a head is listened to after its hello.
    #[cfg(any(windows, test))]
    pub fn end_hello() -> Self {
        Self::Hello {
            role: Role::Head,
            protocol_version: PROTOCOL_VERSION,
            head_id: Some(Self::SESSION_END.into()),
            launched_by_surface: false,
        }
    }

    /// What `thirdeye hook` sends: no head_id, because hooks outnumber heads
    /// by orders of magnitude and have no value to put there.
    pub fn hook_hello() -> Self {
        Self::Hello {
            role: Role::Hook,
            protocol_version: PROTOCOL_VERSION,
            head_id: None,
            launched_by_surface: false,
        }
    }
}

impl TurnView {
    /// Project a core `Turn` into what a head is told about it.
    ///
    /// One row per path, not one per event: two reads of a file are one thing
    /// on screen. The newest flag wins, because an agent that flags a file
    /// twice has changed its mind about why.
    pub fn from_turn(turn: &Turn) -> Self {
        let mut paths: Vec<_> = turn.events.iter().map(|e| e.path.clone()).collect();
        paths.sort();
        paths.dedup();

        let files = paths
            .into_iter()
            .map(|path| {
                let mine = || turn.events.iter().filter(|e| e.path == path);

                let mut kinds: Vec<String> = mine().map(|e| e.kind.label().to_string()).collect();
                kinds.sort();
                kinds.dedup();

                let flag = mine().rev().find_map(|e| match &e.kind {
                    EventKind::Flag { reason, range } => Some(FlagView {
                        reason: reason.clone(),
                        range: *range,
                    }),
                    _ => None,
                });

                // The most recent range wins: it is where the agent last was,
                // which is where a human following along wants to land.
                let range = mine().rev().find_map(|e| e.kind.range());

                FileView {
                    path,
                    kinds,
                    range,
                    flag,
                }
            })
            .collect();

        Self {
            id: turn.prompt_id.clone(),
            prompt: turn.prompt.clone(),
            opened_at: turn.opened_at,
            closed: turn.closed,
            files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{AgentId, FileEvent, LogRecord, Source, group_into_turns};

    fn encode(m: &impl Serialize) -> String {
        serde_json::to_string(m).unwrap()
    }

    #[test]
    fn a_hook_hello_round_trips_as_one_line() {
        // The transport is newline-delimited, so an encoded message carrying
        // a newline would be read as two.
        let encoded = encode(&ClientMessage::hook_hello());
        assert!(!encoded.contains('\n'), "got {encoded}");
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&encoded).unwrap(),
            ClientMessage::hook_hello()
        );
    }

    #[test]
    fn the_wire_names_are_stable() {
        // These strings are the contract with any non-Rust head. Renaming a
        // Rust variant must not silently rename them.
        assert!(encode(&ClientMessage::hook_hello()).contains(r#""message":"hello""#));
        assert!(encode(&Role::Head).contains("head"));
        assert!(
            encode(&ClientMessage::RequestDetail {
                path: "/w/a.rs".into(),
                mode: DetailMode::Diff,
            })
            .contains(r#""mode":"diff""#)
        );
    }

    #[test]
    fn the_policy_names_on_the_wire_are_the_ones_a_human_types() {
        // These are what the slash commands and the spec's table use; a Rust
        // rename must not silently change the vocabulary.
        assert_eq!(encode(&PolicyMode::NotThisTurn), r#""not-this-turn""#);
        assert_eq!(encode(&PolicyMode::Attention), r#""attention""#);
        assert_eq!(encode(&PolicyMode::Any), r#""any""#);
        assert_eq!(encode(&PolicyMode::Never), r#""never""#);
    }

    #[test]
    fn the_default_policy_lets_the_viewer_appear() {
        // A viewer that never showed up unless configured would be a viewer
        // nobody discovers.
        assert_eq!(PolicyMode::default(), PolicyMode::Any);
    }

    #[test]
    fn an_empty_view_state_encodes_to_nothing_at_all() {
        // Sent on every Snapshot and Patch. Empty vectors and Nones are
        // skipped so the common case does not pay for the rare one.
        assert_eq!(encode(&ViewState::default()), "{}");
    }

    #[test]
    fn a_message_from_a_future_daemon_still_parses_its_known_fields() {
        // A stale head meeting a fresh daemon is routine; unknown fields must
        // be ignored rather than fatal, or every added field breaks every
        // deployed head.
        let raw = r#"{"message":"close","reason":"bye","invented_later":42}"#;
        let decoded: ServerMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(
            decoded,
            ServerMessage::Close {
                reason: "bye".into()
            }
        );
    }

    fn event(ts: u64, path: &str, kind: EventKind) -> LogRecord {
        let mut e = FileEvent::new(Timestamp(ts), AgentId::Claude, path, kind, Source::Hook);
        e.prompt_id = Some("p1".into());
        LogRecord::File(e)
    }

    #[test]
    fn one_row_per_file_with_its_kinds_merged() {
        let records = [
            event(1, "/w/a.rs", EventKind::Read { range: None }),
            event(2, "/w/a.rs", EventKind::Write),
            event(3, "/w/b.rs", EventKind::Read { range: None }),
        ];
        let turns = group_into_turns(&records);
        let view = TurnView::from_turn(&turns[0]);

        assert_eq!(view.files.len(), 2, "two reads of one file are one row");
        assert_eq!(view.files[0].path, PathBuf::from("/w/a.rs"));
        assert_eq!(view.files[0].kinds, vec!["read", "write"]);
    }

    #[test]
    fn the_newest_range_is_the_one_a_head_jumps_to() {
        // Where the agent last was, not where it first was.
        let records = [
            event(
                1,
                "/w/a.rs",
                EventKind::Read {
                    range: Some(LineRange::new(1, 10)),
                },
            ),
            event(
                2,
                "/w/a.rs",
                EventKind::Edit {
                    range: Some(LineRange::new(80, 90)),
                },
            ),
        ];
        let turns = group_into_turns(&records);
        let view = TurnView::from_turn(&turns[0]);
        assert_eq!(view.files[0].range, Some(LineRange::new(80, 90)));
    }

    #[test]
    fn a_flag_travels_with_its_reason_and_no_styling() {
        // The head decides what a flag looks like. What crosses the wire is
        // that it is flagged and why.
        let records = [event(
            1,
            "/w/a.rs",
            EventKind::Flag {
                reason: "read this one".into(),
                range: Some(LineRange::new(5, 9)),
            },
        )];
        let turns = group_into_turns(&records);
        let view = TurnView::from_turn(&turns[0]);

        let flag = view.files[0].flag.as_ref().unwrap();
        assert_eq!(flag.reason, "read this one");
        assert_eq!(flag.range, Some(LineRange::new(5, 9)));

        let encoded = encode(&view);
        assert!(
            !encoded.contains('★'),
            "presentation must not leak: {encoded}"
        );
    }

    #[test]
    fn the_newest_reason_for_a_file_is_the_one_sent() {
        let records = [
            event(
                1,
                "/w/a.rs",
                EventKind::Flag {
                    reason: "first thought".into(),
                    range: None,
                },
            ),
            event(
                2,
                "/w/a.rs",
                EventKind::Flag {
                    reason: "actually, this".into(),
                    range: None,
                },
            ),
        ];
        let turns = group_into_turns(&records);
        let view = TurnView::from_turn(&turns[0]);
        assert_eq!(
            view.files[0].flag.as_ref().unwrap().reason,
            "actually, this"
        );
    }

    #[test]
    fn a_hide_from_an_older_client_still_means_until_the_next_turn() {
        // `hide` carried no condition before the human could choose one. A
        // stale `thirdeye close` meeting a fresh daemon is routine — binary
        // and plugin install separately — and must keep its old meaning.
        let old: ClientMessage = serde_json::from_str(r#"{"message":"hide"}"#).unwrap();
        assert_eq!(
            old,
            ClientMessage::Hide {
                until: HideUntil::NextTurn
            }
        );
    }

    #[test]
    fn a_head_is_not_the_surfaces_unless_it_says_so() {
        // Only the surface's own head leaving means its window is gone. A
        // `thirdeye open` disconnecting right after opening one must not read
        // as the human closing it.
        let hello: ClientMessage = serde_json::from_str(
            r#"{"message":"hello","role":"head","protocol_version":1,"head_id":"cli"}"#,
        )
        .unwrap();
        assert!(matches!(
            hello,
            ClientMessage::Hello {
                launched_by_surface: false,
                ..
            }
        ));
    }
}
