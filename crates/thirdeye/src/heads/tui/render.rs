//! Turning head state into a frame.
//!
//! Everything decorative lives here and nowhere else: the `★`, the badges,
//! the fold marks, colours, truncation, the frames and the split. The daemon
//! sent semantics; this is where a human-readable screen is decided, and a
//! different head is free to decide otherwise.
//!
//! Pure with respect to the terminal and the clock — it draws into whatever
//! backend it is given, at whatever `now` it is told, which is how the tests
//! below assert on a real rendered buffer instead of on a description of one.

use std::path::{MAIN_SEPARATOR, Path};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use thirdeye_core::Timestamp;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::state::{HeadState, Row, changed};
use super::theme::Theme;
use crate::daemon::protocol::{DetailMode, FileView, PolicyMode};

/// Below this many columns an open file takes the whole pane: split in two,
/// a side pane leaves both halves too narrow to read.
pub const SIDE_BY_SIDE: u16 = 100;

/// The badges, in the order they are shown. The labels are
/// `EventKind::label`'s.
const BADGES: [(&str, char); 7] = [
    ("edit", 'E'),
    ("write", 'W'),
    ("delete", 'D'),
    ("read", 'R'),
    ("search", 'S'),
    ("exec", 'X'),
    ("mention", 'M'),
];

pub fn draw(state: &HeadState, theme: &Theme, now: Timestamp, frame: &mut Frame) {
    let area = frame.area();
    let [main, keys] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

    match (&state.detail, main.width >= SIDE_BY_SIDE) {
        (None, _) => draw_list(state, theme, now, frame, main),
        (Some(_), true) => {
            let [list, detail] =
                Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .areas(main);
            draw_list(state, theme, now, frame, list);
            draw_detail(state, theme, frame, detail);
        }
        (Some(_), false) => draw_detail(state, theme, frame, main),
    }
    draw_keys(state, theme, frame, keys);
    if state.help {
        draw_help(theme, frame, area);
    }
}

/// The one frame style: thin, rounded, quiet.
fn framed<'a>(theme: &Theme, title: Line<'a>) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme.dim())
        .title(title)
}

fn draw_list(state: &HeadState, theme: &Theme, now: Timestamp, frame: &mut Frame, area: Rect) {
    let workspace = state
        .workspace
        .file_name()
        .map(|n| format!(" · {}", n.to_string_lossy()))
        .unwrap_or_default();
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled("thirdeye", theme.bold()),
        Span::styled(workspace, theme.dim()),
        Span::raw(" "),
    ]);
    let mut footer = vec![format!("policy: {}", policy_label(state.policy))];
    if let Some(agent) = &state.agent {
        footer.insert(0, agent_label(agent).to_string());
    }
    let block = framed(theme, title).title_bottom(
        Line::from(Span::styled(
            format!(" {} ", footer.join(" · ")),
            theme.dim(),
        ))
        .right_aligned(),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if state.turns.is_empty() {
        let empty = Line::from(Span::styled(
            " nothing recorded yet — files appear as the agent works",
            theme.dim(),
        ));
        frame.render_widget(Paragraph::new(empty), inner);
        return;
    }

    let width = inner.width as usize;
    let rows = state.rows();
    let columns = Columns::of(state, &rows, width);
    let mut visual: Vec<(usize, Line)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        for line in row_lines(state, theme, now, row, width, columns) {
            visual.push((index, line));
        }
    }

    // Scroll is computed per frame from the cursor and the height available.
    // It is not state: it depends on the window, which is exactly why §4.3
    // keeps it out of the daemon. A flagged file is two lines, so the window
    // is reckoned in lines, not rows.
    let height = inner.height as usize;
    let first = visual
        .iter()
        .position(|(row, _)| *row == state.cursor)
        .unwrap_or(0);
    let last = visual
        .iter()
        .rposition(|(row, _)| *row == state.cursor)
        .unwrap_or(first);
    let mut offset = visual
        .len()
        .saturating_sub(height)
        .min(first.saturating_sub(height.saturating_sub(1) / 2));
    if last >= offset + height {
        offset = last + 1 - height.max(1);
    }

    let lines: Vec<Line> = visual
        .into_iter()
        .skip(offset)
        .take(height.max(1))
        .map(|(row, line)| {
            if row == state.cursor {
                line.style(theme.cursor())
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// One row of the list as the lines it takes: a flagged file brings its
/// reason on a second line.
fn row_lines<'a>(
    state: &'a HeadState,
    theme: &Theme,
    now: Timestamp,
    row: &Row,
    width: usize,
    columns: Columns,
) -> Vec<Line<'a>> {
    match *row {
        Row::Turn { index } => vec![turn_line(state, theme, now, index, width)],
        Row::File { .. } => {
            let Some(view) = file_of(state, row) else {
                return Vec::new();
            };
            let mut lines = vec![file_line(state, theme, now, view, width, columns)];
            // The reason is the whole content of a flag: a star with no
            // explanation tells the human less than no star would.
            if let Some(flag) = &view.flag {
                let lead = "      └ ";
                let reason = fit(&flag.reason, width.saturating_sub(lead.width()));
                lines.push(Line::from(vec![
                    Span::styled(lead, theme.dim()),
                    Span::styled(
                        pad(&reason, width.saturating_sub(lead.width())),
                        theme.amber(),
                    ),
                ]));
            }
            lines
        }
    }
}

fn turn_line<'a>(
    state: &HeadState,
    theme: &Theme,
    now: Timestamp,
    index: usize,
    width: usize,
) -> Line<'a> {
    let turn = &state.turns[index];
    // A turn still in progress is the one the agent is working on now.
    let (mark, mark_style) = if !turn.closed {
        ("●", theme.amber())
    } else if state.is_collapsed(index) {
        ("▸", theme.dim())
    } else {
        ("▾", theme.dim())
    };
    let number = format!("{:>2}", index + 1);
    let latest = turn
        .files
        .iter()
        .filter_map(|f| f.last_touched)
        .max()
        .unwrap_or(turn.opened_at);
    let right = format!("{:>3}  {:>4}", turn.files.len(), age(now, latest));

    let left_width = 1 + 1 + 1 + number.width() + 2;
    let room = width.saturating_sub(left_width + right.width() + 1);
    let (prompt, prompt_style) = match turn.prompt.as_deref().and_then(|p| p.lines().next()) {
        Some(first) => (first.to_string(), Style::default()),
        None => (
            "(no prompt recorded)".to_string(),
            theme.dim().add_modifier(Modifier::ITALIC),
        ),
    };

    Line::from(vec![
        Span::raw(" "),
        Span::styled(mark, mark_style),
        Span::raw(" "),
        Span::styled(number, theme.bold()),
        Span::raw("  "),
        Span::styled(pad(&fit(&prompt, room), room), prompt_style),
        Span::raw(" "),
        Span::styled(right, theme.dim()),
    ])
}

/// Widths shared by every file row on screen, so names, directories and
/// badges each start in one column down the list.
#[derive(Debug, Clone, Copy)]
struct Columns {
    name: usize,
    badges: usize,
}

impl Columns {
    /// The badge column fits the most badges any row has. The name column
    /// fits the longest name, except names so long that lining the others up
    /// with them would starve every directory: those overrun it alone.
    fn of(state: &HeadState, rows: &[Row], width: usize) -> Self {
        let files: Vec<&FileView> = rows.iter().filter_map(|r| file_of(state, r)).collect();
        let badges = files
            .iter()
            .map(|f| (badges(f).len() * 2).saturating_sub(1))
            .max()
            .unwrap_or(0)
            .max(1);
        let room = width.saturating_sub(STAR + badges + RIGHT_FIXED + 1);
        let name = files
            .iter()
            .map(|f| name_and_dir(state, f).0.width())
            .filter(|&w| w * 5 <= room * 2)
            .max()
            .unwrap_or(0);
        Self { name, badges }
    }
}

/// The star column, `  ★ `.
const STAR: usize = 4;
/// What follows the badges: a space, `⚠`, a space, and the age.
const RIGHT_FIXED: usize = 1 + 1 + 1 + 4;

fn file_of<'s>(state: &'s HeadState, row: &Row) -> Option<&'s FileView> {
    let Row::File { turn, file } = *row else {
        return None;
    };
    let index = *state.ordered_files(turn).get(file)?;
    state.turns.get(turn)?.files.get(index)
}

/// The file's name, and its directory relative to the workspace.
fn name_and_dir(state: &HeadState, view: &FileView) -> (String, String) {
    // By component, never by text: `/w/a` is not a prefix of `/w/abc/x`,
    // and Windows' separator is not `/`.
    let relative = view
        .path
        .strip_prefix(&state.workspace)
        .unwrap_or(&view.path);
    let name = relative
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| relative.display().to_string());
    (name, directory(relative))
}

/// What the agent did to a file, as letters in `BADGES` order.
fn badges(view: &FileView) -> Vec<(&'static str, char)> {
    BADGES
        .iter()
        .filter(|(label, _)| view.kinds.iter().any(|k| k == label))
        .copied()
        .collect()
}

fn file_line<'a>(
    state: &HeadState,
    theme: &Theme,
    now: Timestamp,
    view: &FileView,
    width: usize,
    columns: Columns,
) -> Line<'a> {
    let (star, star_style) = if view.flag.is_some() {
        ("  ★ ", theme.amber())
    } else {
        ("    ", Style::default())
    };
    let (name, dir) = name_and_dir(state, view);

    // Badges, `⚠`, age: a block on the right, each part in its own column.
    let mut right: Vec<Span> = Vec::new();
    for (i, (label, letter)) in badges(view).into_iter().enumerate() {
        if i > 0 {
            right.push(Span::raw(" "));
        }
        let style = match label {
            "edit" | "write" => theme.mint(),
            "delete" => theme.red(),
            _ => theme.cool(),
        };
        right.push(Span::styled(letter.to_string(), style));
    }
    let used: usize = right.iter().map(Span::width).sum();
    right.push(Span::raw(" ".repeat(columns.badges.saturating_sub(used))));
    right.push(Span::raw(" "));
    right.push(if view.stale {
        Span::styled("⚠", theme.amber())
    } else {
        Span::raw(" ")
    });
    right.push(Span::styled(
        format!(
            " {:>4}",
            view.last_touched.map(|t| age(now, t)).unwrap_or_default()
        ),
        theme.dim(),
    ));
    let right_width: usize = right.iter().map(Span::width).sum();

    // The name is what a human scans for, so it gives way last: the
    // directory is shortened from the left, then dropped, then the name cut.
    let room = width.saturating_sub(star.width() + right_width + 1);
    let name_style = if changed(view) {
        theme.bold()
    } else {
        Style::default()
    };
    let mut spans = vec![Span::styled(star, star_style)];
    let name_width = name.width();
    if name_width >= room {
        spans.push(Span::styled(pad(&fit(&name, room), room), name_style));
    } else {
        // The directories start in one column; a name too long for it
        // overruns it, on its own row only.
        let column = columns.name.max(name_width);
        let dir_room = room - column;
        let dir = if dir_room > 2 {
            shorten_left(&dir, dir_room - 2)
        } else {
            String::new()
        };
        spans.push(Span::styled(pad(&name, column), name_style));
        let mut used = column;
        if !dir.is_empty() {
            spans.push(Span::raw("  "));
            used += 2 + dir.width();
            spans.push(Span::styled(dir, theme.dim()));
        }
        spans.push(Span::raw(" ".repeat(room - used)));
    }
    spans.push(Span::raw(" "));
    spans.extend(right);
    Line::from(spans)
}

/// A relative path's directory, with its trailing separator; empty for a
/// file at the top of the workspace.
fn directory(relative: &Path) -> String {
    match relative.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => {
            format!("{}{MAIN_SEPARATOR}", parent.display())
        }
        _ => String::new(),
    }
}

fn draw_detail(state: &HeadState, theme: &Theme, frame: &mut Frame, area: Rect) {
    let Some(detail) = &state.detail else {
        return;
    };
    let relative = detail
        .path
        .strip_prefix(&state.workspace)
        .unwrap_or(&detail.path)
        .display()
        .to_string();
    let diff = detail.mode == DetailMode::Diff;
    let range = match detail.highlight {
        Some(h) if !diff => format!(" · {}–{}", h.start, h.end.saturating_sub(1).max(h.start)),
        _ => String::new(),
    };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(relative, theme.bold()),
        Span::styled(
            format!(" · {}{range} ", if diff { "diff" } else { "content" }),
            theme.dim(),
        ),
    ]);
    let total = detail.lines.len();
    let position = format!(" {}/{total} ", (detail.scroll + 1).min(total));
    let block = framed(theme, title)
        .title_bottom(Line::from(Span::styled(position, theme.dim())).right_aligned());
    let mut inner = block.inner(area);
    frame.render_widget(block, area);

    // A flagged file's reason heads the detail: it is why the human is
    // looking, and the list could only show the start of it.
    let reason = state
        .turns
        .iter()
        .rev()
        .flat_map(|t| &t.files)
        .find(|f| f.path == detail.path)
        .and_then(|f| f.flag.as_ref());
    if let Some(flag) = reason
        && inner.height > 1
    {
        let [top, rest] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("★ ", theme.amber()),
                Span::styled(
                    fit(&flag.reason, (top.width as usize).saturating_sub(2)),
                    theme.amber(),
                ),
            ])),
            top,
        );
        inner = rest;
    }

    let skip = detail.scroll.min(total.saturating_sub(1));
    let last_number = detail.first_line as usize + total.saturating_sub(1);
    let gutter = last_number.to_string().len().max(3);

    // Wrapped by hand, so a long line continues in the text's own column
    // rather than under the line numbers. Only what can show is wrapped.
    let body_width = (inner.width as usize).saturating_sub(if diff { 0 } else { gutter + 2 });
    let mut lines: Vec<Line> = Vec::new();
    for (i, text) in detail.lines.iter().enumerate().skip(skip) {
        if lines.len() >= inner.height as usize {
            break;
        }
        let text = text.replace('\t', "    ");
        let chunks = wrap(&text, body_width);
        if diff {
            // A diff's rows are not file lines; its hunk headers say which
            // lines they are, so it has no gutter of its own.
            let style = if text.starts_with("+++")
                || text.starts_with("---")
                || text.starts_with("diff ")
                || text.starts_with("index ")
            {
                theme.dim()
            } else {
                match text.as_bytes().first() {
                    Some(b'+') => theme.mint(),
                    Some(b'-') => theme.red(),
                    Some(b'@') => theme.cool(),
                    _ => Style::default(),
                }
            };
            lines.extend(
                chunks
                    .into_iter()
                    .map(|c| Line::from(Span::styled(c, style))),
            );
            continue;
        }
        let number = detail.first_line + i as u32;
        let touched = detail
            .highlight
            .is_some_and(|h| number >= h.start && number < h.end);
        let (number_style, bar) = if touched {
            (theme.amber(), "▌")
        } else {
            (theme.dim(), " ")
        };
        for (n, chunk) in chunks.into_iter().enumerate() {
            let label = if n == 0 {
                format!("{number:>gutter$}")
            } else {
                " ".repeat(gutter)
            };
            lines.push(Line::from(vec![
                Span::styled(label, number_style),
                Span::styled(bar, theme.amber()),
                Span::raw(" "),
                Span::raw(chunk),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// `text` broken into pieces of at most `width` columns. Hard breaks: in
/// code, where a line breaks matters less than that nothing is lost.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.width() <= width {
        return vec![text.to_string()];
    }
    let mut pieces = vec![String::new()];
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            pieces.push(String::new());
            used = 0;
        }
        if let Some(last) = pieces.last_mut() {
            last.push(c);
        }
        used += w;
    }
    pieces
}

/// The key line under the frame: what the keys do here, or a message.
fn draw_keys(state: &HeadState, theme: &Theme, frame: &mut Frame, area: Rect) {
    if let Some(status) = &state.status {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {status}"), theme.dim()))),
            area,
        );
        return;
    }
    let keys: &[(&str, &str)] = if state.help {
        &[("?", "close help")]
    } else if let Some(detail) = &state.detail {
        if detail.mode == DetailMode::Diff {
            &[
                ("↑↓", "scroll"),
                ("d", "content"),
                ("esc", "back"),
                ("?", "help"),
            ]
        } else {
            &[
                ("↑↓", "scroll"),
                ("d", "diff"),
                ("esc", "back"),
                ("?", "help"),
            ]
        }
    } else {
        &[("↑↓", "move"), ("⏎", "open"), ("q", "hide"), ("?", "help")]
    };
    let mut spans = vec![Span::raw(" ")];
    for (key, label) in keys {
        spans.push(Span::raw(*key));
        spans.push(Span::styled(format!(" {label}   "), theme.dim()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(theme: &Theme, frame: &mut Frame, area: Rect) {
    let key = |k: &'static str, what: &'static str| {
        Line::from(vec![
            Span::raw(format!("  {k:<9}")),
            Span::styled(what, theme.dim()),
        ])
    };
    let mark = |m: Span<'static>, what: &'static str| {
        Line::from(vec![
            Span::raw("  "),
            m,
            Span::raw("  "),
            Span::styled(what, theme.dim()),
        ])
    };
    let badge = |letter: &'static str, style: Style, what: &'static str| {
        [
            Span::styled(letter, style),
            Span::styled(format!(" {what:<8}"), theme.dim()),
        ]
    };
    let mut changes = vec![Span::raw("  ")];
    changes.extend(badge("E", theme.mint(), "edit"));
    changes.extend(badge("W", theme.mint(), "write"));
    changes.extend(badge("D", theme.red(), "delete"));
    let mut quiet = vec![Span::raw("  ")];
    quiet.extend(badge("R", theme.cool(), "read"));
    quiet.extend(badge("S", theme.cool(), "search"));
    quiet.extend(badge("X", theme.cool(), "command"));
    quiet.extend(badge("M", theme.cool(), "mention"));

    let lines = vec![
        Line::from(Span::styled(" Keys", theme.bold())),
        key("↑↓ j k", "move, or scroll an open file"),
        key("⏎ space", "open a file, or fold a turn"),
        key("d", "a file's diff, or its content"),
        key("esc", "back to the list"),
        key("q", "hide until the next turn's edits"),
        key("m", "hide until the next edit"),
        key("Q", "hide for the rest of the session"),
        key("?", "this help"),
        Line::raw(""),
        Line::from(Span::styled(" Marks", theme.bold())),
        mark(Span::styled("●", theme.amber()), "the turn in progress"),
        mark(
            Span::styled("▸", theme.dim()),
            "a folded turn; ▾ an open one",
        ),
        mark(Span::styled("★", theme.amber()), "flagged by the agent"),
        mark(
            Span::styled("⚠", theme.amber()),
            "changed on disk since the agent read it",
        ),
        Line::from(changes),
        Line::from(quiet),
    ];

    let width = 56.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(framed(
            theme,
            Line::from(Span::styled(" help ", theme.bold())),
        )),
        popup,
    );
}

/// How long ago, in the fewest characters that still read: `now`, `12s`,
/// `3m`, `2h`, `1d`.
fn age(now: Timestamp, then: Timestamp) -> String {
    let seconds = then.elapsed_to(now) / 1000;
    match seconds {
        0..5 => "now".into(),
        5..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// `text` cut to `width` columns, with `…` where it was cut.
fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// A directory cut from the left to `width` columns at whole components,
/// `…` first: the end nearest the file is the part that tells it apart.
/// Empty when not even its last component fits.
fn shorten_left(dir: &str, width: usize) -> String {
    if dir.width() <= width {
        return dir.to_string();
    }
    let parts: Vec<&str> = dir
        .trim_end_matches(MAIN_SEPARATOR)
        .split(MAIN_SEPARATOR)
        .collect();
    let mut kept = String::new();
    for part in parts.iter().rev() {
        let candidate = format!("{part}{MAIN_SEPARATOR}{kept}");
        if 2 + candidate.width() > width {
            break;
        }
        kept = candidate;
    }
    if kept.is_empty() {
        String::new()
    } else {
        format!("…{MAIN_SEPARATOR}{kept}")
    }
}

/// `text` padded with spaces to `width` columns.
fn pad(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
}

fn policy_label(policy: PolicyMode) -> &'static str {
    match policy {
        PolicyMode::Any => "any",
        PolicyMode::Attention => "attention",
        PolicyMode::NotThisTurn => "not-this-turn",
        PolicyMode::Never => "never",
    }
}

/// The product's own name for an agent id the daemon sent.
fn agent_label(agent: &str) -> &str {
    match agent {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        "pi" => "pi",
        "copilot" => "Copilot",
        "antigravity" => "Antigravity",
        "grok" => "Grok",
        "cursor" => "Cursor",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::protocol::{FlagView, PROTOCOL_VERSION, ServerMessage, TurnView};
    use crate::heads::tui::state::Key;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use thirdeye_core::LineRange;

    /// `path` with this platform's separator. A directory is drawn with the
    /// separator the platform's paths use, so what a test expects is too.
    fn native(path: &str) -> String {
        path.replace('/', std::path::MAIN_SEPARATOR_STR)
    }

    /// Ten minutes into the session: every planted timestamp is before it.
    const NOW: Timestamp = Timestamp(600_000);

    fn render(state: &HeadState, theme: &Theme, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(state, theme, NOW, f)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Render into a real buffer and return it as text, one string per row.
    fn screen(state: &HeadState, width: u16, height: u16) -> Vec<String> {
        let buffer = render(state, &Theme::PLAIN, width, height);
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

    fn file(path: &str, kinds: &[&str]) -> FileView {
        FileView {
            path: path.into(),
            kinds: kinds.iter().map(|k| k.to_string()).collect(),
            range: None,
            flag: None,
            last_touched: Some(Timestamp(480_000)),
            stale: false,
        }
    }

    fn flagged(path: &str) -> FileView {
        FileView {
            flag: Some(FlagView {
                reason: "start here".into(),
                range: Some(LineRange::new(1, 4)),
            }),
            ..file(path, &["read"])
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
            agent: Some("claude".into()),
        });
        state
    }

    fn open(state: &mut HeadState, lines: Vec<String>, highlight: Option<LineRange>) {
        state.on_message(ServerMessage::Detail {
            path: "/w/src/a.rs".into(),
            mode: DetailMode::Content,
            first_line: 1,
            lines,
            highlight,
        });
    }

    fn numbered(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("line number {i}")).collect()
    }

    #[test]
    fn the_turn_and_its_files_are_drawn() {
        let state = state_in(&native("/w"), vec![file(&native("/w/src/a.rs"), &["read"])]);
        let screen = screen(&state, 60, 8).join("\n");
        assert!(screen.contains("which should I read?"), "{screen}");
        // The name first, its directory after: the name is what a human
        // scans for. Workspace-relative: the head's choice, which is why the
        // daemon sent paths absolute.
        assert!(screen.contains(&native("a.rs  src/")), "{screen}");
        assert!(!screen.contains(&native("/w/src")), "{screen}");
    }

    #[test]
    fn the_frame_names_the_workspace_the_agent_and_the_policy() {
        let state = state_with(vec![file("/w/src/a.rs", &["read"])]);
        let rows = screen(&state, 60, 8);
        assert!(rows[0].contains("thirdeye · w"), "{rows:#?}");
        assert!(
            rows[6].contains("Claude Code · policy: any"),
            "the bottom edge: {rows:#?}"
        );
    }

    #[test]
    fn a_directory_that_only_starts_with_the_workspaces_name_is_not_inside_it() {
        // Shortened as text, `/w/abc/x.rs` under `/w/a` drew as `bc/x.rs`.
        let state = state_in(
            &native("/w/a"),
            vec![file(&native("/w/abc/x.rs"), &["read"])],
        );
        let screen = screen(&state, 60, 8).join("\n");
        assert!(screen.contains(&native("x.rs  /w/abc/")), "{screen}");
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_path_is_shortened_at_its_own_separator() {
        // Found in a live Zellij pane on Windows: the file drew as `\a.txt`.
        let state = state_in(r"C:\w", vec![file(r"C:\w\src\a.rs", &["read"])]);
        let screen = screen(&state, 60, 8).join("\n");
        assert!(screen.contains(r"a.rs  src\"), "{screen}");
        assert!(!screen.contains(r"\src"), "{screen}");
    }

    #[test]
    fn names_directories_and_badges_each_start_in_one_column() {
        let state = state_in(
            &native("/w"),
            vec![
                file(&native("/w/src/main.rs"), &["edit", "read"]),
                file(&native("/w/tests/a.rs"), &["read"]),
            ],
        );
        let rows = screen(&state, 70, 8);
        let at = |name: &str, needle: &str| {
            let row = rows.iter().find(|r| r.contains(name)).unwrap();
            row[..row.find(needle).unwrap()].chars().count()
        };
        assert_eq!(
            at("main.rs", &native("src/")),
            at("a.rs", &native("tests/")),
            "{rows:#?}"
        );
        assert_eq!(at("main.rs", "E R"), at("a.rs", "R "), "{rows:#?}");
    }

    #[test]
    fn a_flagged_file_is_starred_above_the_rest_with_its_reason_below() {
        let state = state_with(vec![file("/w/a.rs", &["read"]), flagged("/w/z.rs")]);
        let rows = screen(&state, 70, 8);

        let starred = rows.iter().position(|r| r.contains('★')).unwrap();
        let plain = rows.iter().position(|r| r.contains("a.rs")).unwrap();
        assert!(starred < plain, "the flag must sort above: {rows:?}");
        assert!(rows[starred].contains("z.rs"), "{rows:?}");
        assert!(
            rows[starred + 1].contains("└ start here"),
            "the reason is the point: {rows:#?}"
        );
    }

    #[test]
    fn the_turn_in_progress_is_marked_and_a_closed_one_shows_its_fold() {
        let mut state = state_with(vec![file("/w/a.rs", &["read"])]);
        assert!(screen(&state, 60, 8).join("\n").contains("●  1"));

        state.turns[0].closed = true;
        assert!(screen(&state, 60, 8).join("\n").contains("▾  1"));

        state.view.collapsed.push("p1".into());
        let folded = screen(&state, 60, 8).join("\n");
        assert!(folded.contains("▸  1"), "{folded}");
        assert!(!folded.contains("a.rs"), "collapsed files must be hidden");
    }

    #[test]
    fn the_age_is_how_long_ago_the_file_was_touched() {
        let mut state = state_with(vec![file("/w/a.rs", &["read"])]);
        // Planted at 8 minutes, seen at 10.
        let row = |state: &HeadState| {
            screen(state, 60, 8)
                .into_iter()
                .find(|r| r.contains("a.rs"))
                .unwrap()
        };
        assert!(row(&state).contains(" 2m│"), "{}", row(&state));

        // Perturbed: the same file touched just now must not read 2m.
        state.turns[0].files[0].last_touched = Some(Timestamp(599_000));
        assert!(row(&state).contains(" now│"), "{}", row(&state));
    }

    #[test]
    fn ages_read_at_a_glance() {
        let at = |seconds: u64| age(Timestamp(seconds * 1000 + 7), Timestamp(7));
        assert_eq!(at(0), "now");
        assert_eq!(at(12), "12s");
        assert_eq!(at(3 * 60 + 59), "3m");
        assert_eq!(at(2 * 3600), "2h");
        assert_eq!(at(3 * 86_400), "3d");
    }

    #[test]
    fn a_file_changed_behind_the_agent_is_marked_and_only_then() {
        let mut state = state_with(vec![file("/w/a.rs", &["read"])]);
        let marked = |state: &HeadState| screen(state, 60, 8).join("\n").contains('⚠');
        assert!(!marked(&state), "nothing planted, nothing shown");
        state.turns[0].files[0].stale = true;
        assert!(marked(&state));
    }

    #[test]
    fn a_narrow_pane_gives_up_the_directory_before_the_name() {
        let state = state_in(
            &native("/w"),
            vec![file(
                &native("/w/crates/thirdeye/src/heads/tui/render.rs"),
                &["read"],
            )],
        );
        let row = |width| {
            screen(&state, width, 8)
                .into_iter()
                .find(|r| r.contains("rend"))
                .unwrap_or_default()
        };
        assert!(
            row(80).contains(&native("crates/thirdeye/src/heads/tui/")),
            "{}",
            row(80)
        );
        // Shortened from the left at whole components, keeping the end nearest
        // the file.
        assert!(row(45).contains(&native("…/src/heads/tui/")), "{}", row(45));
        assert!(row(45).contains("render.rs"), "{}", row(45));
        // Dropped, then the name cut last.
        assert!(!row(30).contains(std::path::MAIN_SEPARATOR), "{}", row(30));
        assert!(row(22).contains("render…"), "{}", row(22));
    }

    #[test]
    fn a_long_prompt_is_cut_to_fit() {
        let mut state = state_with(vec![file("/w/a.rs", &["read"])]);
        state.turns[0].prompt = Some("x".repeat(200));
        let rows = screen(&state, 60, 8);
        let turn = rows.iter().find(|r| r.contains("●  1")).unwrap();
        assert!(turn.contains('…'), "{turn}");
        assert!(
            turn.contains("1    2m│"),
            "the count and age survive: {turn}"
        );
    }

    #[test]
    fn the_detail_appears_only_once_asked_for() {
        let mut state = state_with(vec![file("/w/src/a.rs", &["read"])]);
        let before = screen(&state, 80, 8).join("\n");
        assert!(!before.contains("esc back"), "{before}");

        open(&mut state, vec!["fn main() {}".into()], None);
        let after = screen(&state, 80, 8).join("\n");
        assert!(after.contains("esc back"), "{after}");
        assert!(after.contains("fn main()"), "{after}");
        assert!(after.contains("src/a.rs · content"), "{after}");
    }

    #[test]
    fn a_narrow_pane_gives_the_whole_of_itself_to_an_open_file() {
        let mut state = state_with(vec![file("/w/src/a.rs", &["read"])]);
        open(&mut state, vec!["fn main() {}".into()], None);

        let narrow = screen(&state, SIDE_BY_SIDE - 1, 8).join("\n");
        assert!(!narrow.contains("which should I read?"), "{narrow}");
        assert!(narrow.contains("fn main()"), "{narrow}");

        let wide = screen(&state, SIDE_BY_SIDE, 8).join("\n");
        assert!(wide.contains("which should I read?"), "{wide}");
        assert!(wide.contains("fn main()"), "{wide}");
    }

    #[test]
    fn the_detail_opens_at_the_lines_the_agent_touched() {
        // The promise: not line one of a 200-line file, but the edit.
        let mut state = state_with(vec![file("/w/src/a.rs", &["edit"])]);
        open(&mut state, numbered(200), Some(LineRange::new(150, 153)));
        let screen = screen(&state, 80, 12).join("\n");
        assert!(screen.contains("line number 150"), "{screen}");
        assert!(!screen.contains("line number 2 "), "{screen}");
        assert!(
            screen.contains("150–152"),
            "the range in the title: {screen}"
        );
        assert!(screen.contains("147/200"), "the position: {screen}");
    }

    #[test]
    fn the_touched_lines_are_marked() {
        let mut state = state_with(vec![file("/w/src/a.rs", &["edit"])]);
        open(&mut state, numbered(200), Some(LineRange::new(150, 152)));
        let rows = screen(&state, 80, 12);
        let marked = |n: u32| {
            rows.iter()
                .find(|r| {
                    r.ends_with(&format!("line number {n}"))
                        || r.contains(&format!("line number {n} "))
                })
                .is_some_and(|r| r.contains('▌'))
        };
        assert!(marked(150) && marked(151), "{rows:#?}");
        assert!(!marked(152), "the range is end-exclusive: {rows:#?}");
    }

    #[test]
    fn a_long_line_continues_in_the_texts_column_not_under_the_numbers() {
        let mut state = state_with(vec![file("/w/src/a.rs", &["read"])]);
        open(
            &mut state,
            vec![format!("start {} end", "x".repeat(60))],
            None,
        );
        let rows = screen(&state, 40, 8);
        let first = rows.iter().position(|r| r.contains("start")).unwrap();
        let text_column = rows[first].find("start").unwrap();
        let next = &rows[first + 1];
        assert!(
            next[..text_column]
                .trim_start_matches('│')
                .trim()
                .is_empty(),
            "{rows:#?}"
        );
        assert!(
            rows.join("\n").contains("end"),
            "nothing is lost: {rows:#?}"
        );
    }

    #[test]
    fn a_flagged_files_reason_heads_its_detail() {
        let mut state = state_with(vec![flagged("/w/src/a.rs")]);
        open(&mut state, vec!["fn main() {}".into()], None);
        let rows = screen(&state, 80, 8);
        assert!(rows[1].contains("★ start here"), "{rows:#?}");
    }

    #[test]
    fn a_diff_shows_no_line_numbers_it_does_not_have() {
        // Numbering a diff's rows would read as file lines; the hunk
        // headers carry the real ones.
        let mut state = state_with(vec![file("/w/src/a.rs", &["edit"])]);
        state.on_message(ServerMessage::Detail {
            path: "/w/src/a.rs".into(),
            mode: DetailMode::Diff,
            first_line: 1,
            lines: vec!["@@ -57,7 +57,7 @@".into(), "-old".into(), "+new".into()],
            highlight: None,
        });
        let rows = screen(&state, 80, 8);
        let row = rows.iter().find(|r| r.contains("+new")).unwrap();
        assert!(!row.contains('3'), "{row}");
        assert!(rows.join("\n").contains("d content"), "{rows:#?}");
    }

    #[test]
    fn without_a_range_the_detail_starts_at_the_top() {
        let mut state = state_with(vec![file("/w/src/a.rs", &["read"])]);
        open(&mut state, numbered(200), None);
        let screen = screen(&state, 80, 12).join("\n");
        assert!(screen.contains("line number 1"), "{screen}");
    }

    #[test]
    fn escape_puts_the_list_back() {
        let mut state = state_with(vec![file("/w/src/a.rs", &["read"])]);
        open(&mut state, vec!["x".into()], None);
        state.on_key(Key::Back);
        let screen = screen(&state, 80, 8).join("\n");
        assert!(!screen.contains("esc back"), "{screen}");
        assert!(screen.contains("which should I read?"), "{screen}");
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_cursor_visible() {
        // The bug this guards: a cursor below the fold leaves the human
        // driving a selection they cannot see. Flags make some rows two
        // lines tall, which a row-counting scroll would get wrong.
        let files: Vec<FileView> = (0..40)
            .map(|i| {
                if i % 3 == 0 {
                    flagged(&format!("/w/file{i:02}.rs"))
                } else {
                    file(&format!("/w/file{i:02}.rs"), &["read"])
                }
            })
            .collect();
        let mut state = state_with(files);
        for _ in 0..39 {
            state.on_key(Key::Down);
        }
        let rows = screen(&state, 60, 10);
        let Row::File { turn, file } = state.rows()[state.cursor] else {
            panic!("expected a file row")
        };
        let order = state.ordered_files(turn);
        let name = state.turns[turn].files[order[file]]
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            rows.iter().any(|r| r.contains(&name)),
            "cursor on {name} must be on screen: {rows:#?}"
        );
    }

    #[test]
    fn an_empty_session_says_so_rather_than_drawing_a_blank_box() {
        let state = HeadState::new();
        let screen = screen(&state, 60, 6).join("\n");
        assert!(screen.contains("nothing recorded yet"), "{screen}");
    }

    #[test]
    fn the_key_line_says_what_the_keys_do_here() {
        let mut state = state_with(vec![file("/w/src/a.rs", &["read"])]);
        let last = |state: &HeadState| screen(state, 60, 8).pop().unwrap();
        assert!(last(&state).contains("⏎ open"), "{}", last(&state));
        open(&mut state, vec!["x".into()], None);
        assert!(last(&state).contains("d diff"), "{}", last(&state));
    }

    #[test]
    fn help_explains_every_key_and_mark() {
        let mut state = state_with(vec![file("/w/src/a.rs", &["read"])]);
        state.on_key(Key::Help);
        let screen = screen(&state, 70, 24).join("\n");
        for needle in [
            "hide for the rest of the session",
            "the turn in progress",
            "flagged by the agent",
            "changed on disk",
            "E edit",
            "M mention",
        ] {
            assert!(screen.contains(needle), "{needle}: {screen}");
        }
    }

    #[test]
    fn colour_marks_what_the_logo_colours_mean() {
        let state = state_with(vec![flagged("/w/z.rs")]);
        let buffer = render(&state, &Theme::TRUECOLOR, 60, 8);
        let star = (0..buffer.area.height)
            .flat_map(|y| (0..buffer.area.width).map(move |x| (x, y)))
            .find(|&p| buffer[p].symbol() == "★")
            .unwrap();
        assert_eq!(buffer[star].fg, Color::Rgb(0xf2, 0xb8, 0x4b));
    }

    #[test]
    fn no_color_means_no_colour_anywhere() {
        let mut state = state_with(vec![
            flagged("/w/z.rs"),
            file("/w/a.rs", &["edit", "delete"]),
        ]);
        state.turns[0].files[1].stale = true;
        let buffer = render(&state, &Theme::PLAIN, 60, 10);
        for cell in buffer.content() {
            assert_eq!(cell.fg, Color::Reset, "{cell:?}");
            assert_eq!(cell.bg, Color::Reset, "{cell:?}");
        }
        // And the cursor is still visible, as reverse video.
        assert!(
            buffer
                .content()
                .iter()
                .any(|c| c.modifier.contains(Modifier::REVERSED))
        );
    }
}
