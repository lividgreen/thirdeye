//! `thirdeye ls` — print the turn-grouped file set for a session.
//!
//! Headless counterpart to the TUI, and the way the capture pipeline is
//! verified before any viewer exists.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Result, bail};
use thirdeye_core::{
    EventKind, EventPaths, LineRange, Turn, Workspace, group_into_turns, read_session,
};

use crate::workspace;

pub fn run(session: Option<String>, explicit_workspace: Option<PathBuf>) -> Result<()> {
    // Resolved through the same path as `thirdeye hook`: reading from a
    // different workspace than the hook wrote to reports an existing session
    // as missing.
    let cwd = std::env::current_dir().ok();
    let ws = workspace::resolve(explicit_workspace, cwd.as_deref());
    run_inner(
        session,
        ws,
        EventPaths::discover,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

/// The command proper, with its path resolution and its output sinks injected
/// so the session-selection branches and the rendering can be tested without
/// touching the user's real state directory or stdout.
fn run_inner(
    session: Option<String>,
    workspace: Workspace,
    make_paths: impl Fn(Workspace) -> EventPaths,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    let paths = make_paths(workspace);

    let session = match session {
        Some(s) => s,
        None => {
            let mut found = paths.sessions()?;
            match found.len() {
                0 => bail!(
                    "no sessions recorded for {}",
                    paths.workspace_dir().display()
                ),
                1 => found.remove(0),
                // Refusing to choose used to be a dead end: the error did not
                // name the ids, so the flag `--session` demanded could not be
                // filled in without going and reading the state directory by
                // hand. Any project accumulates sessions, so this was the
                // normal case, not the edge one. Show the newest — the one a
                // viewer is for — and name the rest.
                _ => {
                    let newest = newest_first(&paths, &mut found);
                    writeln!(
                        err,
                        "note: {} sessions recorded; showing the most recent, {newest}",
                        found.len() + 1
                    )?;
                    writeln!(err, "      others: {} (pass --session)", found.join(", "))?;
                    newest
                }
            }
        }
    };

    let outcome = read_session(&paths, Some(&session))?;
    if outcome.malformed_lines > 0 {
        let n = outcome.malformed_lines;
        writeln!(err, "warning: skipped {n} malformed log {}", lines(n))?;
    }
    if outcome.unknown_records > 0 {
        // Not corruption: a newer producer wrote records this build does not
        // understand. `docs/install.md` symlinks the plugin from a checkout
        // while the binary is installed separately, so a stale binary reading
        // a fresh log is routine. Calling it "malformed" would send the user
        // hunting a bug that does not exist.
        let n = outcome.unknown_records;
        writeln!(
            err,
            "note: skipped {n} log {} written by a newer thirdeye",
            records(n)
        )?;
    }

    let turns = group_into_turns(&outcome.records);
    if turns.is_empty() {
        writeln!(out, "no events recorded for session {session}")?;
        return Ok(());
    }

    for (i, turn) in turns.iter().enumerate() {
        let marker = if turn.closed { " " } else { "*" };
        let label = turn.prompt.as_deref().unwrap_or("(no prompt recorded)");
        writeln!(out, "{marker} turn {} — {}", i + 1, first_line(label, 60))?;

        let mut paths_seen: Vec<_> = turn.events.iter().map(|e| e.path.as_path()).collect();
        paths_seen.sort();
        paths_seen.dedup();
        // Flagged files first. Everything else on this list the agent touched
        // in passing; a flag is the one entry it deliberately asked the human
        // to look at, and burying it alphabetically defeats the point.
        paths_seen.sort_by(|a, b| {
            flag_of(turn, b)
                .is_some()
                .cmp(&flag_of(turn, a).is_some())
                .then_with(|| a.cmp(b))
        });

        for path in paths_seen {
            let kinds: Vec<&str> = {
                let mut k: Vec<&str> = turn
                    .events
                    .iter()
                    .filter(|e| e.path == path)
                    .map(|e| e.kind.label())
                    .collect();
                k.sort_unstable();
                k.dedup();
                k
            };
            let flag = flag_of(turn, path);
            let bullet = if flag.is_some() { "  ★ " } else { "    " };
            writeln!(
                out,
                "{bullet}{:<50} {}",
                display_path(paths.workspace(), path),
                kinds.join(",")
            )?;
            // The reason is the whole content of a flag — a starred path with
            // no explanation tells the human less than not flagging it would.
            if let Some((reason, range)) = flag {
                let where_ = match range {
                    Some(r) if r.len() == 1 => format!(" (line {})", r.start),
                    // Stored end-exclusive, shown inclusive: "lines 40-52" is
                    // what the agent said and what the human will read.
                    Some(r) => format!(" (lines {}-{})", r.start, r.end.saturating_sub(1)),
                    None => String::new(),
                };
                writeln!(out, "        {}{}", first_line(reason, 58), where_)?;
            }
        }
    }
    Ok(())
}

/// Remove and return the most recently written session, leaving the rest in
/// `sessions` ordered newest-first.
///
/// Recency is the mtime of the session's newest segment, not the id: ids are
/// UUIDs, so sorting them alphabetically orders by nothing at all. A session
/// whose mtime cannot be read sorts last rather than erroring — an unreadable
/// directory should not stop `ls` from showing the ones that are fine.
fn newest_first(paths: &EventPaths, sessions: &mut Vec<String>) -> String {
    let written_at = |session: &String| -> Option<SystemTime> {
        std::fs::read_dir(paths.session_dir(Some(session)))
            .ok()?
            .filter_map(|e| e.ok()?.metadata().ok()?.modified().ok())
            .max()
    };
    // Newest first; ties and unreadable sessions fall back to the id so the
    // output is deterministic rather than filesystem-order.
    sessions.sort_by(|a, b| written_at(b).cmp(&written_at(a)).then_with(|| a.cmp(b)));
    sessions.remove(0)
}

/// The flag a turn carries for `path`, if any: its reason and line range.
///
/// The *last* flag wins. An agent that flags a file twice in one turn has
/// changed its mind about why, and the newer reason is the one it would say
/// out loud.
fn flag_of<'t>(turn: &'t Turn, path: &Path) -> Option<(&'t str, Option<LineRange>)> {
    turn.events
        .iter()
        .rev()
        .filter(|e| e.path == path)
        .find_map(|e| match &e.kind {
            EventKind::Flag { reason, range } => Some((reason.as_str(), *range)),
            _ => None,
        })
}

/// Workspace-relative rendering of `path`.
///
/// The workspace root itself relativizes to the empty path — a pathless
/// `Grep`/`Glob` searches exactly that — and an empty column reads as a
/// rendering bug. "." names the root the way a shell would.
fn display_path(workspace: &Workspace, path: &Path) -> String {
    let relative = workspace.relativize(path);
    if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative.display().to_string()
    }
}

fn lines(n: usize) -> &'static str {
    if n == 1 { "line" } else { "lines" }
}

fn records(n: usize) -> &'static str {
    if n == 1 { "record" } else { "records" }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or_default();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{
        AgentId, EventKind, FileEvent, LogRecord, Source, Timestamp, TurnKind, TurnMarker, Writer,
    };

    const WS: &str = "/w";

    struct Harness {
        state: PathBuf,
        runtime: PathBuf,
    }

    fn harness(tag: &str) -> Harness {
        let base = std::env::temp_dir().join(format!("thirdeye-ls-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        Harness {
            state: base.join("state"),
            runtime: base.join("run"),
        }
    }

    impl Harness {
        fn paths(&self) -> EventPaths {
            EventPaths::with_roots(&self.state, &self.runtime, Workspace::new(WS))
        }

        fn write(&self, session: &str, records: &[LogRecord]) {
            let mut w = Writer::open(&self.paths(), Some(session), "hook").unwrap();
            for r in records {
                w.append(r).unwrap();
            }
        }

        /// Runs `ls` and returns `(stdout, stderr)`.
        fn run(&self, session: Option<&str>) -> Result<(String, String)> {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let (state, runtime) = (self.state.clone(), self.runtime.clone());
            run_inner(
                session.map(str::to_owned),
                Workspace::new(WS),
                move |ws| EventPaths::with_roots(state.clone(), runtime.clone(), ws),
                &mut out,
                &mut err,
            )?;
            Ok((
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            ))
        }
    }

    /// Push every segment in `dir` to a plainly-later mtime.
    ///
    /// Two sessions written in the same test can share an mtime at the
    /// filesystem's resolution, which would make the recency assertions pass
    /// or fail by coincidence.
    fn touch_later(dir: &Path) {
        let later = SystemTime::now() + std::time::Duration::from_secs(60);
        for entry in std::fs::read_dir(dir).unwrap() {
            let file = std::fs::OpenOptions::new()
                .append(true)
                .open(entry.unwrap().path())
                .unwrap();
            file.set_modified(later).unwrap();
        }
    }

    fn open(ts: u64, id: &str, prompt: &str) -> LogRecord {
        LogRecord::Turn(
            TurnMarker::new(
                Timestamp(ts),
                id,
                TurnKind::Open {
                    prompt: prompt.into(),
                },
            )
            .with_session("s1"),
        )
    }

    fn close(ts: u64, id: &str) -> LogRecord {
        LogRecord::Turn(TurnMarker::new(Timestamp(ts), id, TurnKind::Close).with_session("s1"))
    }

    fn event(ts: u64, path: &str, kind: EventKind, id: &str) -> LogRecord {
        let mut ev = FileEvent::new(Timestamp(ts), AgentId::Claude, path, kind, Source::Hook);
        ev.prompt_id = Some(id.into());
        LogRecord::File(ev)
    }

    #[test]
    fn no_recorded_sessions_is_an_error_naming_the_workspace_looked_in() {
        // The message is the user's only clue that `ls` and `hook` disagreed
        // about the workspace, so it must carry the directory.
        let h = harness("none");
        let err = h.run(None).unwrap_err().to_string();
        assert!(err.contains("no sessions recorded"), "got {err}");
        assert!(
            err.contains(&h.paths().workspace_dir().display().to_string()),
            "got {err}"
        );
    }

    #[test]
    fn a_single_recorded_session_is_selected_without_being_named() {
        let h = harness("one");
        h.write("s1", &[open(1, "p1", "fix dedup")]);
        let (out, _) = h.run(None).unwrap();
        assert!(out.contains("fix dedup"), "got {out}");
    }

    #[test]
    fn several_recorded_sessions_show_the_newest_and_name_the_rest() {
        // This used to be an error that named no ids, so the `--session` it
        // demanded could not be supplied without reading the state directory
        // by hand — and a project accumulates sessions, so it was the common
        // path, not the rare one.
        let h = harness("many");
        h.write("s1", &[open(1, "p1", "older session")]);
        h.write("s2", &[open(1, "p2", "newer session")]);
        // mtime decides, so make the ordering real rather than incidental.
        touch_later(&h.paths().session_dir(Some("s2")));

        let (out, err) = h.run(None).unwrap();
        assert!(out.contains("newer session"), "got {out}");
        assert!(err.contains("2 sessions recorded"), "got {err}");
        assert!(err.contains("s2"), "the shown session must be named: {err}");
        assert!(err.contains("s1"), "the others must be named: {err}");
        assert!(err.contains("--session"), "got {err}");

        // Naming one still wins over the guess, silently.
        let (out, err) = h.run(Some("s1")).unwrap();
        assert!(out.contains("older session"), "got {out}");
        assert_eq!(err, "", "an explicit choice needs no commentary");
    }

    #[test]
    fn the_newest_session_is_chosen_by_mtime_not_by_id() {
        // Ids are UUIDs. Alphabetical order over them is meaningless, and
        // would silently show whichever session happened to sort first.
        let h = harness("recency");
        h.write("zzz-newest", &[open(1, "p1", "the current one")]);
        h.write("aaa-oldest", &[open(1, "p2", "the stale one")]);
        touch_later(&h.paths().session_dir(Some("zzz-newest")));

        let (out, _) = h.run(None).unwrap();
        assert!(out.contains("the current one"), "got {out}");
    }

    #[test]
    fn output_groups_paths_under_their_turn_with_merged_kind_labels() {
        let h = harness("render");
        h.write(
            "s1",
            &[
                open(1, "p1", "first prompt"),
                event(2, "/w/src/log.rs", EventKind::Read { range: None }, "p1"),
                event(3, "/w/src/log.rs", EventKind::Write, "p1"),
                close(4, "p1"),
                open(5, "p2", "second prompt"),
                event(6, "/w/src/main.rs", EventKind::Write, "p2"),
            ],
        );

        let (out, err) = h.run(None).unwrap();
        assert_eq!(err, "", "a clean log must produce no warnings");

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "got {out}");
        // A closed turn is blank-marked; the open one is the current turn.
        assert!(lines[0].starts_with("  turn 1 — first prompt"), "{out}");
        assert!(lines[2].starts_with("* turn 2 — second prompt"), "{out}");
        // Paths render workspace-relative, one line per path, kinds merged:
        // two events on one file are one row, not two.
        assert!(lines[1].trim_start().starts_with("src/log.rs"), "{out}");
        assert!(lines[1].ends_with("read,write"), "{out}");
        assert!(lines[3].trim_start().starts_with("src/main.rs"), "{out}");
    }

    fn flag(ts: u64, path: &str, reason: &str, range: Option<LineRange>, id: &str) -> LogRecord {
        let mut ev = FileEvent::new(
            Timestamp(ts),
            AgentId::Claude,
            path,
            EventKind::Flag {
                reason: reason.into(),
                range,
            },
            Source::Mcp,
        );
        ev.prompt_id = Some(id.into());
        LogRecord::File(ev)
    }

    #[test]
    fn a_flagged_file_sorts_above_the_files_merely_touched() {
        // Alphabetically z.rs is last. The agent asked for it to be read, so
        // it goes first — that ordering is the whole feature.
        let h = harness("flag-order");
        h.write(
            "s1",
            &[
                open(1, "p1", "which one should I read?"),
                event(2, "/w/a.rs", EventKind::Read { range: None }, "p1"),
                event(3, "/w/m.rs", EventKind::Write, "p1"),
                flag(4, "/w/z.rs", "this is the one worth reading", None, "p1"),
            ],
        );

        let (out, _) = h.run(None).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].contains('★'), "flag must be marked: {out}");
        assert!(lines[1].contains("z.rs"), "{out}");
        assert_eq!(lines[2].trim(), "this is the one worth reading");
        assert!(lines[3].contains("a.rs"), "{out}");
        assert!(lines[4].contains("m.rs"), "{out}");
    }

    #[test]
    fn a_flag_renders_its_range_the_way_the_agent_said_it() {
        // Stored end-exclusive, shown inclusive. Rendering the raw `end`
        // would tell the human to read one line further than the agent meant.
        let h = harness("flag-range");
        h.write(
            "s1",
            &[
                open(1, "p1", "review the parser"),
                flag(
                    2,
                    "/w/parse.rs",
                    "the bounds check is off here",
                    Some(LineRange::new(40, 53)),
                    "p1",
                ),
                flag(
                    3,
                    "/w/one.rs",
                    "this single line",
                    Some(LineRange::new(7, 8)),
                    "p1",
                ),
            ],
        );

        let (out, _) = h.run(None).unwrap();
        assert!(out.contains("(lines 40-52)"), "got {out}");
        assert!(
            out.contains("(line 7)"),
            "a one-line range must not read as a span: {out}"
        );
    }

    #[test]
    fn the_newest_reason_for_a_file_is_the_one_shown() {
        // Two flags on one file are one row; showing the stale reason would
        // contradict what the agent last said.
        let h = harness("flag-newest");
        h.write(
            "s1",
            &[
                open(1, "p1", "look again"),
                flag(2, "/w/a.rs", "first thought", None, "p1"),
                flag(3, "/w/a.rs", "actually, this is why", None, "p1"),
            ],
        );

        let (out, _) = h.run(None).unwrap();
        assert!(out.contains("actually, this is why"), "got {out}");
        assert!(!out.contains("first thought"), "got {out}");
        assert_eq!(out.lines().count(), 3, "one row plus its reason: {out}");
    }

    #[test]
    fn a_search_of_the_workspace_root_renders_as_a_dot_not_a_blank_column() {
        // A pathless Grep/Glob is attributed to the workspace root, which
        // relativizes to the empty path. An empty column reads as a bug.
        let h = harness("root-path");
        h.write(
            "s1",
            &[
                open(1, "p1", "search everything"),
                event(
                    2,
                    WS,
                    EventKind::Search {
                        pattern: "**/*.rs".into(),
                    },
                    "p1",
                ),
            ],
        );
        let (out, _) = h.run(None).unwrap();
        let row = out.lines().nth(1).unwrap();
        assert!(row.trim_start().starts_with(". "), "got {out:?}");
    }

    #[test]
    fn records_this_build_cannot_read_are_reported_as_stale_not_as_damage() {
        let h = harness("future");
        h.write("s1", &[open(1, "p1", "hello")]);
        let seg = h.paths().segment(Some("s1"), "newer");
        std::fs::write(&seg, "{\"record\":\"snapshot\",\"ts\":9}\n").unwrap();

        let (_, err) = h.run(None).unwrap();
        assert!(err.contains("written by a newer thirdeye"), "got {err:?}");
        assert!(
            !err.contains("malformed"),
            "a newer record is not corruption: {err:?}"
        );
        // Singular, because "1 log records" reads as a bug in the tool.
        assert!(err.contains("skipped 1 log record "), "got {err:?}");
    }

    #[test]
    fn a_session_with_no_records_says_so_rather_than_printing_nothing() {
        let h = harness("empty");
        // Session directory exists but holds no parsable records.
        std::fs::create_dir_all(h.paths().session_dir(Some("s1"))).unwrap();
        let (out, _) = h.run(None).unwrap();
        assert!(out.contains("no events recorded for session s1"), "{out}");
    }
}
