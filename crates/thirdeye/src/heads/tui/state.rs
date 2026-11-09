//! What the head knows and how it reacts.
//!
//! Deliberately free of ratatui and of any terminal: this is a state machine
//! from `(ServerMessage | Key)` to `(new state, messages to send)`. Rendering
//! reads it, the event loop drives it, and neither is needed to test it.
//!
//! **Presentation lives here, not in the daemon** (§4.3). The daemon says
//! *this file is flagged and here is why*; the decisions that flags sort first
//! and are marked `★` are the head's, and a web head may reach different ones.

use std::path::PathBuf;

use crate::daemon::protocol::{
    ClientMessage, DetailMode, HideUntil, PolicyMode, ServerMessage, TurnView, ViewState,
};

/// A key press, in the head's own vocabulary.
///
/// Not crossterm's type: the event loop translates, so every test below can
/// press a key without a terminal existing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Activate,
    Back,
    /// Show the open file's diff, or its content again.
    ToggleDiff,
    /// Leave, telling the daemon when the viewer may come back.
    Hide(HideUntil),
    /// Leave without saying; the daemon applies its default.
    Quit,
}

/// One line in the list pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Turn {
        index: usize,
    },
    File {
        turn: usize,
        /// Index into that turn's files *as displayed*, which is not the order
        /// the daemon sent them in.
        file: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub path: PathBuf,
    pub first_line: u32,
    pub lines: Vec<String>,
    /// What the daemon actually sent — content when a diff was asked for
    /// but there was none.
    pub mode: DetailMode,
    /// The lines the agent touched: where the view opens, and what it marks.
    pub highlight: Option<thirdeye_core::LineRange>,
    /// Index of the first line shown. Starts a few lines above the touched
    /// range; the arrow keys move it while a file is open.
    pub scroll: usize,
}

/// Lines shown above the touched range when a file opens, so the change sits
/// in context rather than at the very top edge.
const CONTEXT: usize = 3;

/// Where a file opens: a few lines above the touched range, or the top —
/// including when the range is past the end of a file that has since shrunk.
fn opening_scroll(
    first_line: u32,
    lines: usize,
    highlight: Option<thirdeye_core::LineRange>,
) -> usize {
    highlight
        .and_then(|h| h.start.checked_sub(first_line))
        .map(|i| i as usize)
        .filter(|&i| i < lines)
        .map_or(0, |i| i.saturating_sub(CONTEXT))
}

pub struct HeadState {
    pub session: String,
    pub workspace: PathBuf,
    pub turns: Vec<TurnView>,
    /// The daemon's view state, echoed back to us. Authoritative: a second
    /// head changing the selection must move this one too.
    pub view: ViewState,
    /// What the daemon says its policy is, so the head can show it. The head
    /// never decides policy; it only asks for changes.
    pub policy: PolicyMode,
    pub detail: Option<Detail>,
    /// Cursor over visible rows. Scroll offset is deliberately absent — it
    /// depends on the window size, which is why §4.3 keeps it out of the
    /// daemon; here it is the renderer's business, recomputed per frame.
    pub cursor: usize,
    pub status: Option<String>,
    pub should_quit: bool,
}

impl HeadState {
    pub fn new() -> Self {
        Self {
            session: String::new(),
            workspace: PathBuf::new(),
            turns: Vec::new(),
            view: ViewState::default(),
            policy: PolicyMode::default(),
            detail: None,
            cursor: 0,
            status: None,
            should_quit: false,
        }
    }

    /// Files of `turn`, in display order: flagged first, then by path.
    ///
    /// The daemon sorts by path alone, because ordering by attention is a
    /// presentation decision. A flagged file is the one entry the agent
    /// deliberately asked the human to look at; burying it alphabetically
    /// defeats the point of flagging it.
    pub fn ordered_files(&self, turn: usize) -> Vec<usize> {
        let Some(turn) = self.turns.get(turn) else {
            return Vec::new();
        };
        let mut order: Vec<usize> = (0..turn.files.len()).collect();
        order.sort_by(|&a, &b| {
            let (a, b) = (&turn.files[a], &turn.files[b]);
            b.flag
                .is_some()
                .cmp(&a.flag.is_some())
                .then_with(|| a.path.cmp(&b.path))
        });
        order
    }

    pub fn is_collapsed(&self, turn: usize) -> bool {
        self.turns
            .get(turn)
            .and_then(|t| t.id.as_ref())
            .is_some_and(|id| self.view.collapsed.contains(id))
    }

    /// Every row currently on screen, top to bottom.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for index in 0..self.turns.len() {
            rows.push(Row::Turn { index });
            if !self.is_collapsed(index) {
                for file in 0..self.ordered_files(index).len() {
                    rows.push(Row::File { turn: index, file });
                }
            }
        }
        rows
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.rows().get(self.cursor).cloned()
    }

    /// Fold new state in. Returns anything that must be sent back.
    pub fn on_message(&mut self, message: ServerMessage) -> Vec<ClientMessage> {
        match message {
            ServerMessage::Snapshot {
                session,
                workspace,
                turns,
                view,
                policy,
                ..
            } => {
                self.session = session;
                self.workspace = workspace;
                self.turns = turns;
                self.view = view;
                self.policy = policy;
            }
            ServerMessage::Patch {
                turns,
                view,
                policy,
            } => {
                if !turns.is_empty() {
                    self.turns = turns;
                }
                if let Some(view) = view {
                    self.view = view;
                }
                if let Some(policy) = policy {
                    self.policy = policy;
                }
            }
            ServerMessage::Detail {
                path,
                first_line,
                lines,
                mode,
                highlight,
            } => {
                let scroll = opening_scroll(first_line, lines.len(), highlight);
                self.detail = Some(Detail {
                    path,
                    first_line,
                    lines,
                    mode,
                    highlight,
                    scroll,
                });
            }
            ServerMessage::Close { reason } => {
                self.status = Some(format!("daemon closed the connection: {reason}"));
                self.should_quit = true;
            }
        }
        // The log grew while the cursor was near the bottom: clamp rather
        // than let it point past the end.
        let rows = self.rows().len();
        if self.cursor >= rows {
            self.cursor = rows.saturating_sub(1);
        }
        Vec::new()
    }

    /// The connection ended without a `Close`.
    pub fn on_daemon_gone(&mut self) {
        self.status = Some("the daemon has gone".into());
        self.should_quit = true;
    }

    pub fn on_key(&mut self, key: Key) -> Vec<ClientMessage> {
        match key {
            Key::Quit => {
                self.should_quit = true;
                Vec::new()
            }
            Key::Hide(until) => {
                self.should_quit = true;
                vec![ClientMessage::Hide { until }]
            }
            // The open file, the other way round. The daemon decides whether
            // a diff exists; it answers with content, labelled so, if not.
            Key::ToggleDiff => match &self.detail {
                Some(detail) => vec![ClientMessage::RequestDetail {
                    path: detail.path.clone(),
                    mode: match detail.mode {
                        DetailMode::Diff => DetailMode::Content,
                        DetailMode::Content => DetailMode::Diff,
                    },
                }],
                None => Vec::new(),
            },
            Key::Back => {
                // Closing the detail pane is local: the daemon does not track
                // whether a head happens to be showing one.
                self.detail = None;
                Vec::new()
            }
            // With a file open the arrows scroll it: opening at the touched
            // lines hides what is above them, and a long diff runs past the
            // pane. The list keeps its cursor for when the file is closed.
            Key::Up if self.detail.is_some() => {
                if let Some(d) = &mut self.detail {
                    d.scroll = d.scroll.saturating_sub(1);
                }
                Vec::new()
            }
            Key::Down if self.detail.is_some() => {
                if let Some(d) = &mut self.detail {
                    d.scroll = (d.scroll + 1).min(d.lines.len().saturating_sub(1));
                }
                Vec::new()
            }
            Key::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Vec::new()
            }
            Key::Down => {
                let last = self.rows().len().saturating_sub(1);
                self.cursor = (self.cursor + 1).min(last);
                Vec::new()
            }
            Key::Activate => self.activate(),
        }
    }

    fn activate(&mut self) -> Vec<ClientMessage> {
        match self.selected_row() {
            Some(Row::Turn { index }) => {
                let Some(id) = self.turns.get(index).and_then(|t| t.id.clone()) else {
                    // The ungrouped bucket has no id, so there is nothing the
                    // daemon could record about folding it.
                    return Vec::new();
                };
                let expanded = self.is_collapsed(index);
                vec![ClientMessage::ExpandTurn { turn: id, expanded }]
            }
            Some(Row::File { turn, file }) => {
                let Some(path) = self
                    .ordered_files(turn)
                    .get(file)
                    .and_then(|&i| self.turns[turn].files.get(i))
                    .map(|f| f.path.clone())
                else {
                    return Vec::new();
                };
                // Selection is durable state the daemon owns; the detail is
                // ours to ask for. Both go in one activation because a human
                // pressing enter means "show me this".
                vec![
                    ClientMessage::Select { path: path.clone() },
                    ClientMessage::RequestDetail {
                        path,
                        mode: DetailMode::Content,
                    },
                ]
            }
            None => Vec::new(),
        }
    }
}

impl Default for HeadState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::{FileView, FlagView, PROTOCOL_VERSION};
    use thirdeye_core::{LineRange, Timestamp};

    fn file(path: &str, flagged: bool) -> FileView {
        FileView {
            path: path.into(),
            kinds: vec!["read".into()],
            range: None,
            flag: flagged.then(|| FlagView {
                reason: "look here".into(),
                range: Some(LineRange::new(1, 4)),
            }),
        }
    }

    fn turn(id: &str, files: Vec<FileView>) -> TurnView {
        TurnView {
            id: Some(id.into()),
            prompt: Some(format!("prompt {id}")),
            opened_at: Timestamp(1),
            closed: false,
            files,
        }
    }

    fn snapshot(turns: Vec<TurnView>) -> ServerMessage {
        ServerMessage::Snapshot {
            protocol_version: PROTOCOL_VERSION,
            session: "s1".into(),
            workspace: "/w".into(),
            turns,
            view: ViewState::default(),
            policy: PolicyMode::default(),
        }
    }

    fn with(turns: Vec<TurnView>) -> HeadState {
        let mut state = HeadState::new();
        state.on_message(snapshot(turns));
        state
    }

    #[test]
    fn a_flagged_file_is_shown_above_the_files_merely_touched() {
        // Alphabetically z.rs is last; the agent asked for it, so it is first.
        let state = with(vec![turn(
            "p1",
            vec![file("/w/a.rs", false), file("/w/z.rs", true)],
        )]);
        let order = state.ordered_files(0);
        assert_eq!(
            state.turns[0].files[order[0]].path,
            PathBuf::from("/w/z.rs")
        );
    }

    #[test]
    fn collapsing_a_turn_hides_its_files_but_not_its_header() {
        let mut state = with(vec![turn("p1", vec![file("/w/a.rs", false)])]);
        assert_eq!(state.rows().len(), 2);

        state.view.collapsed.push("p1".into());
        assert_eq!(state.rows(), vec![Row::Turn { index: 0 }]);
    }

    #[test]
    fn activating_a_turn_asks_to_toggle_it() {
        let mut state = with(vec![turn("p1", vec![file("/w/a.rs", false)])]);
        state.cursor = 0;

        let sent = state.on_key(Key::Activate);
        assert_eq!(
            sent,
            vec![ClientMessage::ExpandTurn {
                turn: "p1".into(),
                expanded: false
            }]
        );

        // Folded, the same key asks to unfold.
        state.view.collapsed.push("p1".into());
        assert_eq!(
            state.on_key(Key::Activate),
            vec![ClientMessage::ExpandTurn {
                turn: "p1".into(),
                expanded: true
            }]
        );
    }

    #[test]
    fn activating_a_file_selects_it_and_asks_for_its_contents() {
        let mut state = with(vec![turn("p1", vec![file("/w/a.rs", false)])]);
        state.cursor = 1;

        let sent = state.on_key(Key::Activate);
        assert_eq!(
            sent,
            vec![
                ClientMessage::Select {
                    path: "/w/a.rs".into()
                },
                ClientMessage::RequestDetail {
                    path: "/w/a.rs".into(),
                    mode: DetailMode::Content
                },
            ]
        );
    }

    #[test]
    fn the_cursor_cannot_walk_off_either_end() {
        let mut state = with(vec![turn("p1", vec![file("/w/a.rs", false)])]);
        for _ in 0..5 {
            state.on_key(Key::Up);
        }
        assert_eq!(state.cursor, 0);
        for _ in 0..10 {
            state.on_key(Key::Down);
        }
        assert_eq!(state.cursor, state.rows().len() - 1);
    }

    #[test]
    fn a_patch_that_shrinks_the_list_pulls_the_cursor_back_in() {
        // Turns do not usually vanish, but a head must not index past the end
        // if one does — that is a panic in the middle of the user's terminal.
        let mut state = with(vec![
            turn("p1", vec![file("/w/a.rs", false)]),
            turn("p2", vec![file("/w/b.rs", false)]),
        ]);
        state.cursor = state.rows().len() - 1;

        state.on_message(ServerMessage::Patch {
            turns: vec![turn("p1", vec![])],
            view: None,
            policy: None,
        });
        assert!(state.cursor < state.rows().len());
    }

    #[test]
    fn an_empty_patch_does_not_wipe_the_list() {
        // `Patch { turns: [] }` is how a view-only change travels. Treating it
        // as "there are now no turns" would blank the screen on every
        // selection change.
        let mut state = with(vec![turn("p1", vec![file("/w/a.rs", false)])]);
        state.on_message(ServerMessage::Patch {
            turns: Vec::new(),
            view: Some(ViewState {
                selected: Some("/w/a.rs".into()),
                collapsed: Vec::new(),
            }),
            policy: None,
        });
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.view.selected, Some("/w/a.rs".into()));
    }

    #[test]
    fn the_daemon_closing_the_connection_ends_the_head() {
        let mut state = with(vec![]);
        state.on_message(ServerMessage::Close {
            reason: "session ended".into(),
        });
        assert!(state.should_quit);
        assert!(state.status.as_deref().unwrap().contains("session ended"));
    }

    #[test]
    fn each_hide_key_names_its_own_condition_and_leaves() {
        // The daemon closes the window this head lives in, so the head goes
        // either way; saying which condition first is the only chance it has.
        for (key, until) in [
            (Key::Hide(HideUntil::NextTurn), HideUntil::NextTurn),
            (Key::Hide(HideUntil::NextMutation), HideUntil::NextMutation),
            (Key::Hide(HideUntil::Session), HideUntil::Session),
        ] {
            let mut state = with(vec![]);
            assert_eq!(state.on_key(key), vec![ClientMessage::Hide { until }]);
            assert!(state.should_quit);
        }
    }

    #[test]
    fn quitting_outright_says_nothing() {
        // ctrl-c is "get me out", not a choice of condition. The daemon sees
        // the surface's head leave and applies the default itself.
        let mut state = with(vec![]);
        assert!(state.on_key(Key::Quit).is_empty());
        assert!(state.should_quit);
    }

    #[test]
    fn a_daemon_that_vanished_ends_the_head() {
        // SIGTERM gives the daemon no chance to send `Close`. A head left
        // running would sit in its pane showing a session that has ended.
        let mut state = with(vec![]);
        state.on_daemon_gone();
        assert!(state.should_quit);
    }

    fn open_detail(state: &mut HeadState, mode: DetailMode) {
        state.on_message(ServerMessage::Detail {
            path: "/w/a.rs".into(),
            mode,
            first_line: 1,
            lines: vec!["x".into()],
            highlight: Some(thirdeye_core::LineRange::new(3, 5)),
        });
    }

    #[test]
    fn the_detail_remembers_where_the_agent_touched_it_and_how_it_is_shown() {
        let mut state = with(vec![]);
        open_detail(&mut state, DetailMode::Content);
        let detail = state.detail.as_ref().unwrap();
        assert_eq!(detail.highlight, Some(thirdeye_core::LineRange::new(3, 5)));
        assert_eq!(detail.mode, DetailMode::Content);
    }

    #[test]
    fn d_toggles_the_open_file_between_content_and_diff() {
        let mut state = with(vec![]);
        open_detail(&mut state, DetailMode::Content);
        assert_eq!(
            state.on_key(Key::ToggleDiff),
            vec![ClientMessage::RequestDetail {
                path: "/w/a.rs".into(),
                mode: DetailMode::Diff
            }]
        );
        open_detail(&mut state, DetailMode::Diff);
        assert_eq!(
            state.on_key(Key::ToggleDiff),
            vec![ClientMessage::RequestDetail {
                path: "/w/a.rs".into(),
                mode: DetailMode::Content
            }]
        );
    }

    fn open_long(state: &mut HeadState, highlight: Option<thirdeye_core::LineRange>, lines: usize) {
        state.on_message(ServerMessage::Detail {
            path: "/w/a.rs".into(),
            mode: DetailMode::Content,
            first_line: 1,
            lines: (1..=lines).map(|i| format!("line {i}")).collect(),
            highlight,
        });
    }

    #[test]
    fn with_a_file_open_the_arrows_scroll_it_not_the_list() {
        // Opening at the touched lines hides what is above them; without
        // scrolling, those lines — and the rest of a long diff — are
        // unreachable.
        let mut state = with(vec![]);
        open_long(&mut state, Some(thirdeye_core::LineRange::new(50, 52)), 100);
        let cursor = state.cursor;
        assert_eq!(
            state.detail.as_ref().unwrap().scroll,
            46,
            "three lines of context above line 50"
        );
        state.on_key(Key::Down);
        assert_eq!(state.detail.as_ref().unwrap().scroll, 47);
        for _ in 0..100 {
            state.on_key(Key::Up);
        }
        assert_eq!(state.detail.as_ref().unwrap().scroll, 0);
        for _ in 0..500 {
            state.on_key(Key::Down);
        }
        assert_eq!(
            state.detail.as_ref().unwrap().scroll,
            99,
            "never past the last line"
        );
        assert_eq!(state.cursor, cursor, "the list cursor stays where it was");
    }

    #[test]
    fn a_range_past_the_end_of_a_shrunken_file_opens_at_the_top() {
        let mut state = with(vec![]);
        open_long(&mut state, Some(thirdeye_core::LineRange::new(50, 52)), 3);
        assert_eq!(state.detail.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn d_with_no_file_open_does_nothing() {
        assert!(with(vec![]).on_key(Key::ToggleDiff).is_empty());
    }

    #[test]
    fn the_ungrouped_bucket_cannot_be_folded() {
        // It has no id, so there is nothing the daemon could record.
        let mut state = with(vec![TurnView {
            id: None,
            prompt: None,
            opened_at: Timestamp(1),
            closed: false,
            files: vec![file("/w/a.rs", false)],
        }]);
        state.cursor = 0;
        assert!(state.on_key(Key::Activate).is_empty());
    }
}
