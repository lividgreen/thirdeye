//! Grouping log records into conversational turns.
//!
//! Turns come from `prompt_id`, opened by `UserPromptSubmit` and closed by
//! `Stop`. Grouping is by id rather than by position so that out-of-order
//! records and a missing opening marker both degrade gracefully — installing
//! thirdeye mid-session must not lose events.

use crate::event::{FileEvent, LogRecord, Timestamp, TurnKind};

/// One conversational turn and every file event attributed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// `None` for events an adapter could not attribute to a turn.
    pub prompt_id: Option<String>,
    /// `None` when the opening marker was never seen.
    pub prompt: Option<String>,
    pub opened_at: Timestamp,
    /// A turn with no `Stop` is the current one.
    pub closed: bool,
    pub events: Vec<FileEvent>,
}

/// Group records into turns, in first-appearance order.
pub fn group_into_turns(records: &[LogRecord]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();

    // Linear scan over the turn list: a session has few turns, so this stays
    // cheaper than a map plus an ordering index.
    fn slot<'t>(turns: &'t mut Vec<Turn>, id: Option<&str>, ts: Timestamp) -> &'t mut Turn {
        let existing = turns.iter().position(|t| t.prompt_id.as_deref() == id);
        match existing {
            Some(i) => &mut turns[i],
            None => {
                turns.push(Turn {
                    prompt_id: id.map(str::to_owned),
                    prompt: None,
                    opened_at: ts,
                    closed: false,
                    events: Vec::new(),
                });
                turns.last_mut().expect("just pushed")
            }
        }
    }

    for record in records {
        match record {
            LogRecord::Turn(marker) => {
                let turn = slot(&mut turns, Some(&marker.prompt_id), marker.ts);
                match &marker.kind {
                    TurnKind::Open { prompt } => {
                        // Antigravity opens turns but hands its hooks no
                        // prompt text: an empty prompt is no prompt, and
                        // every head already says so for one.
                        turn.prompt = Some(prompt.clone()).filter(|p| !p.trim().is_empty());
                        turn.opened_at = marker.ts;
                    }
                    TurnKind::Close => turn.closed = true,
                }
            }
            LogRecord::File(event) => {
                let turn = slot(&mut turns, event.prompt_id.as_deref(), event.ts);
                turn.events.push(event.clone());
            }
            // A record this build cannot interpret carries no turn to group
            // it under. `read_session` filters these out and counts them;
            // this arm only keeps a hand-built slice from panicking.
            LogRecord::Unknown => {}
        }
    }

    turns
}

/// The turn a newly-produced event should join: the most recently opened one
/// that has not been closed.
///
/// Hooks get `prompt_id` handed to them in the payload. A producer that is not
/// a hook — the MCP server — is called mid-turn with no such field, and an
/// event with no `prompt_id` lands in the leading ungrouped turn, which is
/// exactly where a flag must *not* appear: the point of flagging a file is to
/// attach it to the work the human just asked for.
///
/// Reading it back off the log rather than tracking it in memory keeps this
/// correct across a producer that starts mid-session, and keeps the log the
/// single source of truth for turn state.
///
/// `None` when no turn is open — the agent acted before any `UserPromptSubmit`
/// was recorded, or thirdeye was installed mid-turn. Guessing the last *closed*
/// turn would attach the event to work that already finished.
pub fn open_turn_id(records: &[LogRecord]) -> Option<String> {
    group_into_turns(records)
        .into_iter()
        .rev()
        .find(|t| !t.closed && t.prompt_id.is_some())
        .and_then(|t| t.prompt_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentId, EventKind, FileEvent, Source, TurnMarker};

    fn open(ts: u64, id: &str) -> LogRecord {
        LogRecord::Turn(
            TurnMarker::new(
                Timestamp(ts),
                id,
                TurnKind::Open {
                    prompt: format!("prompt {id}"),
                },
            )
            .with_session("s1"),
        )
    }

    fn close(ts: u64, id: &str) -> LogRecord {
        LogRecord::Turn(TurnMarker::new(Timestamp(ts), id, TurnKind::Close).with_session("s1"))
    }

    fn edit(ts: u64, path: &str, id: Option<&str>) -> LogRecord {
        let mut ev = FileEvent::new(
            Timestamp(ts),
            AgentId::Claude,
            path,
            EventKind::Edit { range: None },
            Source::Hook,
        );
        ev.prompt_id = id.map(str::to_owned);
        LogRecord::File(ev)
    }

    #[test]
    fn events_are_grouped_under_their_prompt_id() {
        let turns = group_into_turns(&[
            open(1, "p1"),
            edit(2, "/a.rs", Some("p1")),
            close(3, "p1"),
            open(4, "p2"),
            edit(5, "/b.rs", Some("p2")),
        ]);

        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].prompt_id.as_deref(), Some("p1"));
        assert_eq!(turns[0].events.len(), 1);
        assert!(turns[0].closed);
        assert_eq!(turns[1].prompt_id.as_deref(), Some("p2"));
        assert!(!turns[1].closed, "an unclosed turn is the current one");
    }

    #[test]
    fn a_file_touched_in_two_turns_appears_in_both() {
        // The turn view is a history, not a deduplicated set.
        let turns = group_into_turns(&[
            open(1, "p1"),
            edit(2, "/a.rs", Some("p1")),
            open(3, "p2"),
            edit(4, "/a.rs", Some("p2")),
        ]);
        assert_eq!(turns[0].events[0].path, turns[1].events[0].path);
        assert_eq!(turns.len(), 2);
    }

    #[test]
    fn a_turn_opened_without_prompt_text_has_no_prompt() {
        let open = TurnMarker::new(
            Timestamp(1),
            "21",
            TurnKind::Open {
                prompt: String::new(),
            },
        );
        let turns = group_into_turns(&[LogRecord::Turn(open)]);
        assert_eq!(turns[0].prompt, None);
    }

    #[test]
    fn events_without_a_prompt_id_land_in_a_leading_ungrouped_turn() {
        // An adapter that cannot supply a turn id degrades, never drops data.
        let turns = group_into_turns(&[
            edit(1, "/a.rs", None),
            open(2, "p1"),
            edit(3, "/b.rs", Some("p1")),
        ]);

        assert_eq!(turns.len(), 2);
        assert!(turns[0].prompt_id.is_none());
        assert_eq!(turns[0].events.len(), 1);
        assert_eq!(turns[1].prompt_id.as_deref(), Some("p1"));
    }

    #[test]
    fn an_event_whose_turn_was_never_opened_still_groups() {
        // Installing mid-session means the opening marker is missing.
        let turns = group_into_turns(&[edit(1, "/a.rs", Some("p9"))]);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].prompt_id.as_deref(), Some("p9"));
        assert!(turns[0].prompt.is_none());
    }

    #[test]
    fn an_event_arriving_before_its_own_open_marker_still_joins_that_turn() {
        // The module doc claims grouping is id-keyed, not position-keyed.
        // This is reachable in practice: the plugin's hooks are `"async"`, so
        // a PostToolUse process can win the race against the UserPromptSubmit
        // process that opens the turn, and equal-millisecond timestamps sort
        // arbitrarily besides.
        let turns = group_into_turns(&[
            edit(2, "/a.rs", Some("p1")),
            open(1, "p1"),
            edit(3, "/b.rs", Some("p1")),
            close(4, "p1"),
        ]);

        assert_eq!(turns.len(), 1, "the late marker must not fork a turn");
        assert_eq!(turns[0].prompt_id.as_deref(), Some("p1"));
        assert_eq!(turns[0].events.len(), 2);
        assert_eq!(
            turns[0].prompt.as_deref(),
            Some("prompt p1"),
            "the prompt must attach even though its marker arrived second"
        );
        assert!(turns[0].closed);
        assert_eq!(
            turns[0].opened_at,
            Timestamp(1),
            "opened_at must come from the marker, not from the first event seen"
        );
    }

    #[test]
    fn unknown_records_are_ignored_rather_than_forming_a_turn() {
        let turns = group_into_turns(&[LogRecord::Unknown, open(1, "p1"), LogRecord::Unknown]);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].prompt_id.as_deref(), Some("p1"));
    }

    #[test]
    fn empty_input_yields_no_turns() {
        assert!(group_into_turns(&[]).is_empty());
    }

    #[test]
    fn the_open_turn_is_the_one_a_new_event_joins() {
        let records = [open(1, "p1"), close(2, "p1"), open(3, "p2")];
        assert_eq!(open_turn_id(&records).as_deref(), Some("p2"));
    }

    #[test]
    fn a_closed_session_offers_no_turn_to_join() {
        // Attaching to the last *closed* turn would file the flag under work
        // the human already moved on from.
        let records = [open(1, "p1"), edit(2, "/a.rs", Some("p1")), close(3, "p1")];
        assert_eq!(open_turn_id(&records), None);
    }

    #[test]
    fn an_out_of_order_close_still_closes_its_own_turn() {
        // Hooks are async, so a Stop marker can land before a tool-use record
        // written earlier. Grouping is id-keyed, and this must be too.
        let records = [open(1, "p1"), close(2, "p1"), edit(3, "/a.rs", Some("p1"))];
        assert_eq!(open_turn_id(&records), None);
    }

    #[test]
    fn unattributed_events_offer_no_turn_to_join() {
        // The ungrouped turn has no id, so there is nothing to join. Returning
        // it would mean flagging into the bucket flags exist to stay out of.
        let records = [edit(1, "/a.rs", None)];
        assert_eq!(open_turn_id(&records), None);
    }

    #[test]
    fn a_turn_known_only_from_its_events_is_still_joinable() {
        // Installing thirdeye mid-turn means the opening marker was never
        // written. The tool calls still name their prompt_id, and that is
        // enough — degrading to "ungrouped" here would be a worse answer than
        // the one the log plainly supports.
        let records = [edit(1, "/a.rs", Some("p1"))];
        assert_eq!(open_turn_id(&records).as_deref(), Some("p1"));
    }
}
