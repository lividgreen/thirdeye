//! Turning head state into a frame.
//!
//! Everything decorative lives here and nowhere else: the `★`, the fold
//! arrows, colours, truncation, the split. The daemon sent semantics; this is
//! where a human-readable screen is decided, and a different head is free to
//! decide otherwise.
//!
//! Pure with respect to the terminal — it draws into whatever backend it is
//! given, which is how the tests below assert on a real rendered buffer
//! instead of on a description of one.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::state::{HeadState, Row};
use crate::daemon::protocol::DetailMode;

pub fn draw(state: &HeadState, frame: &mut Frame) {
    let area = frame.area();
    // The detail pane only exists once the human has asked for one, so an
    // untouched head is a full-width list rather than half a screen of
    // emptiness.
    let panes = if state.detail.is_some() {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(area)
    };

    draw_list(state, frame, panes[0]);
    if state.detail.is_some() {
        draw_detail(state, frame, panes[1]);
    }
}

fn draw_list(state: &HeadState, frame: &mut Frame, area: Rect) {
    let rows = state.rows();
    let inner_height = area.height.saturating_sub(2) as usize;

    // Scroll is computed per frame from the cursor and the height available.
    // It is not state: it depends on the window, which is exactly why §4.3
    // keeps it out of the daemon.
    let offset = rows.len().saturating_sub(inner_height).min(
        state
            .cursor
            .saturating_sub(inner_height.saturating_sub(1) / 2),
    );

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(inner_height.max(1))
        .map(|(index, row)| render_row(state, row, index == state.cursor))
        .collect();

    let title = if state.turns.is_empty() {
        " thirdeye — nothing recorded yet ".to_string()
    } else {
        format!(" thirdeye — {} ", short_session(&state.session))
    };

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn render_row<'a>(state: &'a HeadState, row: &Row, is_cursor: bool) -> Line<'a> {
    let cursor_style = if is_cursor {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };

    match *row {
        Row::Turn { index } => {
            let turn = &state.turns[index];
            let arrow = if state.is_collapsed(index) {
                "▶"
            } else {
                "▼"
            };
            // A turn still in progress is the one the agent is working on now.
            let marker = if turn.closed { " " } else { "*" };
            let prompt = turn
                .prompt
                .as_deref()
                .unwrap_or("(no prompt recorded)")
                .lines()
                .next()
                .unwrap_or_default();
            Line::from(vec![
                Span::styled(
                    format!("{marker}{arrow} turn {} ", index + 1),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(prompt.to_string()),
            ])
            .style(cursor_style)
        }
        Row::File { turn, file } => {
            let order = state.ordered_files(turn);
            let view = &state.turns[turn].files[order[file]];
            let flagged = view.flag.is_some();
            let bullet = if flagged { "  ★ " } else { "    " };

            // By component, never by text: `/w/a` is not a prefix of
            // `/w/abc/x`, and Windows' separator is not `/`.
            let name = view
                .path
                .strip_prefix(&state.workspace)
                .unwrap_or(&view.path)
                .display()
                .to_string();

            let mut spans = vec![
                Span::styled(
                    bullet,
                    if flagged {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    },
                ),
                Span::raw(name),
                Span::styled(
                    format!("  {}", view.kinds.join(",")),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            // The reason is the whole content of a flag: a star with no
            // explanation tells the human less than no star would.
            if let Some(flag) = &view.flag {
                spans.push(Span::styled(
                    format!("  — {}", flag.reason),
                    Style::default().fg(Color::Yellow),
                ));
            }
            Line::from(spans).style(cursor_style)
        }
    }
}

fn draw_detail(state: &HeadState, frame: &mut Frame, area: Rect) {
    let Some(detail) = &state.detail else {
        return;
    };
    let name = detail
        .path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| detail.path.display().to_string());

    let skip = detail.scroll.min(detail.lines.len().saturating_sub(1));
    let diff = detail.mode == DetailMode::Diff;

    let lines: Vec<Line> = detail
        .lines
        .iter()
        .enumerate()
        .skip(skip)
        .map(|(i, text)| {
            let number = detail.first_line + i as u32;
            let touched = detail
                .highlight
                .is_some_and(|h| number >= h.start && number < h.end);
            let gutter = if diff {
                // A diff's rows are not file lines; its hunk headers say
                // which lines they are.
                Span::raw("  ")
            } else if touched {
                Span::styled(format!("{number:>5}▌"), Style::default().fg(Color::Yellow))
            } else {
                Span::styled(
                    format!("{number:>5} "),
                    Style::default().fg(Color::DarkGray),
                )
            };
            let body = if diff {
                let colour = match text.as_bytes().first() {
                    Some(b'+') if !text.starts_with("+++") => Color::Green,
                    Some(b'-') if !text.starts_with("---") => Color::Red,
                    Some(b'@') => Color::Cyan,
                    _ => Color::Reset,
                };
                Span::styled(text.clone(), Style::default().fg(colour))
            } else {
                Span::raw(text.clone())
            };
            Line::from(vec![gutter, body])
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::default().borders(Borders::ALL).title(format!(
                " {name}{} — d: {} · esc: back ",
                if diff { " (diff)" } else { "" },
                if diff { "content" } else { "diff" }
            )),
        ),
        area,
    );
}

/// Session ids are UUIDs; the first segment is enough to tell two apart.
fn short_session(session: &str) -> &str {
    session.split('-').next().unwrap_or(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::{FileView, FlagView, PROTOCOL_VERSION, ServerMessage, TurnView};
    use crate::heads::tui::state::Key;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use thirdeye_core::{LineRange, Timestamp};

    /// Render into a real buffer and return it as text, one string per row.
    fn screen(state: &HeadState, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(state, f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn file(path: &str, flagged: bool) -> FileView {
        FileView {
            path: path.into(),
            kinds: vec!["read".into()],
            range: None,
            flag: flagged.then(|| FlagView {
                reason: "start here".into(),
                range: Some(LineRange::new(1, 4)),
            }),
        }
    }

    fn state_with(files: Vec<FileView>) -> HeadState {
        state_in("/w", files)
    }

    fn state_in(workspace: &str, files: Vec<FileView>) -> HeadState {
        let mut state = HeadState::new();
        state.on_message(ServerMessage::Snapshot {
            protocol_version: PROTOCOL_VERSION,
            session: "abc12345-0000-0000-0000-000000000000".into(),
            workspace: workspace.into(),
            turns: vec![TurnView {
                id: Some("p1".into()),
                prompt: Some("which should I read?".into()),
                opened_at: Timestamp(1),
                closed: false,
                files,
            }],
            view: Default::default(),
            policy: Default::default(),
        });
        state
    }

    #[test]
    fn the_turn_and_its_files_are_drawn() {
        let state = state_with(vec![file("/w/src/a.rs", false)]);
        let screen = screen(&state, 60, 8).join("\n");
        assert!(screen.contains("turn 1"), "{screen}");
        assert!(screen.contains("which should I read?"), "{screen}");
        // Paths render workspace-relative: that is the head's choice, which
        // is why the daemon sent them absolute.
        assert!(screen.contains("src/a.rs"), "{screen}");
        assert!(!screen.contains("/w/src/a.rs"), "{screen}");
    }

    #[test]
    fn a_directory_that_only_starts_with_the_workspaces_name_is_not_inside_it() {
        // Shortened as text, `/w/abc/x.rs` under `/w/a` drew as `bc/x.rs`.
        let state = state_in("/w/a", vec![file("/w/abc/x.rs", false)]);
        let screen = screen(&state, 60, 8).join("\n");
        assert!(screen.contains("/w/abc/x.rs"), "{screen}");
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_path_is_shortened_at_its_own_separator() {
        // Found in a live Zellij pane on Windows: the file drew as `\a.txt`.
        let state = state_in(r"C:\w", vec![file(r"C:\w\src\a.rs", false)]);
        let screen = screen(&state, 60, 8).join("\n");
        assert!(screen.contains(r"src\a.rs"), "{screen}");
        assert!(!screen.contains(r"\src"), "{screen}");
    }

    #[test]
    fn a_flagged_file_is_starred_above_the_rest_with_its_reason() {
        let state = state_with(vec![file("/w/a.rs", false), file("/w/z.rs", true)]);
        let rows = screen(&state, 70, 8);

        let starred = rows.iter().position(|r| r.contains('★')).unwrap();
        let plain = rows.iter().position(|r| r.contains("a.rs")).unwrap();
        assert!(starred < plain, "the flag must sort above: {rows:?}");
        assert!(rows[starred].contains("z.rs"), "{rows:?}");
        assert!(
            rows[starred].contains("start here"),
            "the reason is the point"
        );
    }

    #[test]
    fn a_collapsed_turn_shows_its_arrow_and_hides_its_files() {
        let mut state = state_with(vec![file("/w/a.rs", false)]);
        state.view.collapsed.push("p1".into());
        let screen = screen(&state, 60, 8).join("\n");
        assert!(screen.contains('▶'), "{screen}");
        assert!(!screen.contains("a.rs"), "collapsed files must be hidden");
    }

    #[test]
    fn the_detail_pane_appears_only_once_asked_for() {
        let mut state = state_with(vec![file("/w/a.rs", false)]);
        let before = screen(&state, 80, 8).join("\n");
        assert!(!before.contains("esc: back"), "{before}");

        state.on_message(ServerMessage::Detail {
            path: "/w/a.rs".into(),
            mode: crate::daemon::protocol::DetailMode::Content,
            first_line: 1,
            lines: vec!["fn main() {}".into()],
            highlight: None,
        });
        let after = screen(&state, 80, 8).join("\n");
        assert!(after.contains("esc: back"), "{after}");
        assert!(after.contains("fn main()"), "{after}");
        // Gutter numbers start from what the daemon said, not from zero.
        assert!(after.contains('1'), "{after}");
    }

    fn long_detail(highlight: Option<thirdeye_core::LineRange>) -> HeadState {
        let mut state = state_with(vec![file("/w/a.rs", false)]);
        state.on_message(ServerMessage::Detail {
            path: "/w/a.rs".into(),
            mode: crate::daemon::protocol::DetailMode::Content,
            first_line: 1,
            lines: (1..=200).map(|i| format!("line number {i}")).collect(),
            highlight,
        });
        state
    }

    #[test]
    fn the_detail_opens_at_the_lines_the_agent_touched() {
        // The promise: not line one of a 200-line file, but the edit.
        let range = thirdeye_core::LineRange::new(150, 153);
        let screen = screen(&long_detail(Some(range)), 80, 12).join("\n");
        assert!(screen.contains("line number 150"), "{screen}");
        assert!(!screen.contains("line number 1\n"), "{screen}");
        assert!(!screen.contains("line number 2 "), "{screen}");
    }

    #[test]
    fn the_touched_lines_are_marked() {
        let range = thirdeye_core::LineRange::new(150, 152);
        let rows = screen(&long_detail(Some(range)), 80, 12);
        let marked = |n: u32| {
            rows.iter()
                .find(|r| {
                    r.contains(&format!("line number {n} "))
                        || r.ends_with(&format!("line number {n}"))
                })
                .is_some_and(|r| r.contains('▌'))
        };
        assert!(marked(150) && marked(151), "{rows:#?}");
        assert!(!marked(152), "the range is end-exclusive: {rows:#?}");
    }

    #[test]
    fn a_diff_shows_no_line_numbers_it_does_not_have() {
        // Numbering a diff's rows would read as file lines; the hunk
        // headers carry the real ones.
        let mut state = state_with(vec![file("/w/a.rs", false)]);
        state.on_message(ServerMessage::Detail {
            path: "/w/a.rs".into(),
            mode: crate::daemon::protocol::DetailMode::Diff,
            first_line: 1,
            lines: vec!["@@ -57,7 +57,7 @@".into(), "-old".into(), "+new".into()],
            highlight: None,
        });
        let rows = screen(&state, 80, 8);
        let row = rows.iter().find(|r| r.contains("+new")).unwrap();
        assert!(!row.contains(" 3 "), "{row}");
    }

    #[test]
    fn without_a_range_the_detail_starts_at_the_top() {
        let screen = screen(&long_detail(None), 80, 12).join("\n");
        assert!(screen.contains("line number 1"), "{screen}");
    }

    #[test]
    fn escape_puts_the_list_back_to_full_width() {
        let mut state = state_with(vec![file("/w/a.rs", false)]);
        state.on_message(ServerMessage::Detail {
            path: "/w/a.rs".into(),
            mode: crate::daemon::protocol::DetailMode::Content,
            first_line: 1,
            lines: vec!["x".into()],
            highlight: None,
        });
        state.on_key(Key::Back);
        assert!(!screen(&state, 80, 8).join("\n").contains("esc: back"));
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_cursor_visible() {
        // The bug this guards: a cursor below the fold leaves the human
        // driving a selection they cannot see.
        let files: Vec<FileView> = (0..40)
            .map(|i| file(&format!("/w/file{i:02}.rs"), false))
            .collect();
        let mut state = state_with(files);
        for _ in 0..39 {
            state.on_key(Key::Down);
        }
        let rows = screen(&state, 60, 10);
        let cursor_row = &state.rows()[state.cursor];
        let Row::File { turn, file } = cursor_row else {
            panic!("expected a file row")
        };
        let order = state.ordered_files(*turn);
        let name = state.turns[*turn].files[order[*file]]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            rows.iter().any(|r| r.contains(&name)),
            "cursor on {name} must be on screen: {rows:?}"
        );
    }

    #[test]
    fn an_empty_session_says_so_rather_than_drawing_a_blank_box() {
        let state = HeadState::new();
        let screen = screen(&state, 60, 6).join("\n");
        assert!(screen.contains("nothing recorded yet"), "{screen}");
    }
}
