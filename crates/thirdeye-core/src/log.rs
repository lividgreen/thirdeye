//! The append-only, session-keyed event log.
//!
//! The log *is* the interface between producers and heads — there is no
//! daemon. Hooks are short-lived shell commands and need a drop point that
//! exists whether or not a viewer is running; a file is that drop point, with
//! no lifecycle, no port, and no events lost while the TUI is closed.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::event::{FileEvent, LogRecord};
use crate::location::{EventPaths, Workspace};

/// Ceiling on one serialized event, newline included.
///
/// Kept at `PIPE_BUF` (4096 on Linux) so a single `O_APPEND` write stays
/// atomic. This is not an optimization: the plugin runs its hooks with
/// `"async": true`, so several `thirdeye hook` processes append to one
/// segment concurrently, and an oversized write can interleave into a torn —
/// or worse, plausible-but-wrong — line. [`Writer::append`] therefore
/// *enforces* this rather than making a best effort at it.
pub const MAX_LINE_BYTES: usize = 4096;

/// Byte budget handed to each individually-clippable field.
///
/// A quarter of the line leaves room for several long fields on one record
/// plus the fixed envelope.
const FIELD_BUDGET: usize = MAX_LINE_BYTES / 4;

/// Smallest path tail worth keeping. Below this the path says nothing, so
/// the record is refused instead.
const MIN_PATH_TAIL: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("event log I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not serialize event: {0}")]
    Encode(#[from] serde_json::Error),
    /// Every clippable field was clipped and the line still does not fit.
    ///
    /// Only reachable when a record's *identity* fields (session id,
    /// `prompt_id`, `dedup_key`) are themselves pathological. Dropping one
    /// record is strictly better than emitting a write that can interleave
    /// with a concurrent producer's.
    #[error("event still {len} bytes after truncation, over the {MAX_LINE_BYTES}-byte line cap")]
    Oversized { len: usize },
}

/// `create_dir_all` with `0700` on every directory it creates.
fn create_private_dir_all(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    // The mode above applies only to directories this call creates. Tighten
    // the session dir unconditionally so a directory left world-readable by
    // an earlier build is corrected rather than inherited forever.
    restrict(dir, 0o700)
}

/// Set `mode` on an existing path. A no-op off Unix.
///
/// Tightening is best-effort *only* for a filesystem that cannot represent
/// Unix permissions at all — FAT/exFAT, many network mounts, a state dir
/// inherited from another owner — which fail `chmod` with `EPERM`/`EACCES`
/// or `ENOTSUP`. Propagating those made [`Writer::open`] fatal there, and
/// since `thirdeye-hook.sh` deliberately swallows a failing hook into
/// `hook-errors.log`, capture went *silently* dead for the whole session:
/// no error in the agent, just an `ls` that never shows anything. A log the
/// user can read is worth more than no log at all.
///
/// What is given up is narrow. New segments are still created `0600` through
/// `OpenOptions::mode` and new directories `0700` through `DirBuilder::mode`,
/// so only the *correction* of an already-loose path is skipped — and on a
/// filesystem with no permission bits there was nothing to correct. Every
/// other error still propagates: a failing `chmod` on a filesystem that does
/// support modes is real I/O trouble and must not be mistaken for this.
#[allow(unused_variables)]
fn restrict(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            Err(e) if !permissions_unsupported(&e) => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

/// Whether `e` means "this filesystem does not do Unix permissions" rather
/// than "the operation failed".
#[cfg(unix)]
fn permissions_unsupported(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
    )
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> LogError + '_ {
    move |source| LogError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Appends one producer's events to its own segment.
pub struct Writer {
    path: PathBuf,
    file: File,
    workspace: Workspace,
}

impl Writer {
    /// Opens (creating as needed) the segment for `producer` in `session`.
    ///
    /// The log holds full user prompt text and every path the agent touched,
    /// so on Unix the session directory is `0700` and the segment `0600` —
    /// the same posture the spec requires of the runtime dir. The default
    /// umask leaves both group- and world-readable, which on a shared host
    /// means any local user can read them.
    pub fn open(
        paths: &EventPaths,
        session: Option<&str>,
        producer: &str,
    ) -> Result<Self, LogError> {
        let path = paths.segment(session, producer);
        let dir = path.parent().expect("segment path always has a parent");
        create_private_dir_all(dir).map_err(io_err(dir))?;

        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(io_err(&path))?;
        restrict(&path, 0o600).map_err(io_err(&path))?;

        Ok(Self {
            path,
            file,
            workspace: paths.workspace().clone(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one record as a single JSON line, never exceeding
    /// [`MAX_LINE_BYTES`].
    ///
    /// The size cap is enforced against the *encoded* line, so it holds for
    /// every record shape rather than only for the fields a truncation pass
    /// happens to know about. A record that does not fit is shrunk in
    /// escalating steps: first the descriptive fields (shell commands, search
    /// patterns, flag reasons, open prompts) — agents emit very long `Bash`
    /// commands and losing that detail beats losing the record — then the
    /// path, keeping its tail, which is the end that identifies the file.
    /// A record that still does not fit is refused rather than written.
    pub fn append(&mut self, record: &LogRecord) -> Result<(), LogError> {
        // Here, because every producer appends through here: the log then
        // holds one spelling of each file, and nothing that reads it has to
        // know that Windows ignores case. See `Workspace::respell`.
        let respelled;
        let record = match record {
            LogRecord::File(event) => match self.workspace.respell(&event.path) {
                Some(path) => {
                    let mut event = event.clone();
                    event.path = path;
                    respelled = LogRecord::File(event);
                    &respelled
                }
                None => record,
            },
            _ => record,
        };
        let mut line = encode_within_cap(record)?;
        line.push('\n');
        debug_assert!(line.len() <= MAX_LINE_BYTES);
        self.file
            .write_all(line.as_bytes())
            .map_err(io_err(&self.path))?;
        self.file.flush().map_err(io_err(&self.path))
    }
}

/// Serialize `record` to a line body guaranteed to leave room for a newline
/// within [`MAX_LINE_BYTES`].
fn encode_within_cap(record: &LogRecord) -> Result<String, LogError> {
    // The trailing newline occupies the last byte of the budget.
    let fits = |line: &str| line.len() < MAX_LINE_BYTES;

    let line = serde_json::to_string(record)?;
    if fits(&line) {
        return Ok(line);
    }

    let mut shrunk = record.clone();
    truncate_unbounded_fields(&mut shrunk, FIELD_BUDGET);
    let line = serde_json::to_string(&shrunk)?;
    if fits(&line) {
        return Ok(line);
    }

    // The path is the remaining unbounded field. Halve the tail we keep
    // until the encoded line fits: JSON escaping means the encoded size is
    // not a fixed function of the path's byte length, so measure rather than
    // compute.
    let mut tail = FIELD_BUDGET;
    while tail >= MIN_PATH_TAIL {
        let mut candidate = shrunk.clone();
        if let LogRecord::File(event) = &mut candidate {
            keep_path_tail(&mut event.path, tail);
        }
        let line = serde_json::to_string(&candidate)?;
        if fits(&line) {
            return Ok(line);
        }
        tail /= 2;
    }

    Err(LogError::Oversized {
        len: serde_json::to_string(&shrunk)?.len() + 1,
    })
}

fn truncate_unbounded_fields(record: &mut LogRecord, budget: usize) {
    use crate::event::{EventKind, TurnKind};
    let clip = |s: &mut String| {
        if s.len() > budget {
            s.truncate(s.floor_char_boundary(budget));
            s.push('…');
        }
    };
    match record {
        LogRecord::File(event) => match &mut event.kind {
            EventKind::Exec { cmd } => clip(cmd),
            EventKind::Search { pattern } => clip(pattern),
            EventKind::Flag { reason, .. } => clip(reason),
            EventKind::Read { .. }
            | EventKind::Edit { .. }
            | EventKind::Write
            | EventKind::Delete
            | EventKind::Mention => {}
        },
        LogRecord::Turn(marker) => {
            if let TurnKind::Open { prompt } = &mut marker.kind {
                clip(prompt)
            }
        }
        // Nothing to clip: the payload was discarded at parse time, and this
        // build never writes one.
        LogRecord::Unknown => {}
    }
}

/// Clip a path to its last `budget` bytes, marking the cut with a leading
/// ellipsis. The tail is kept because the basename is what identifies the
/// file to a human; the leading directories are the redundant part.
fn keep_path_tail(path: &mut PathBuf, budget: usize) {
    let clipped = {
        let text = path.to_string_lossy();
        if text.len() <= budget {
            return;
        }
        let cut = (text.len() - budget..=text.len())
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(text.len());
        format!("…{}", &text[cut..])
    };
    *path = PathBuf::from(clipped);
}

/// Outcome of reading a session's segments.
#[derive(Debug, Default)]
pub struct ReadOutcome {
    /// Merged, timestamp-ordered, deduplicated records.
    pub records: Vec<LogRecord>,
    /// Lines that failed to parse.
    ///
    /// Surfaced rather than swallowed: a reader tailing a file a producer is
    /// mid-write on will legitimately see a torn final line, but a *rising*
    /// count means an adapter is emitting garbage, and a silent skip would
    /// hide that indefinitely.
    pub malformed_lines: usize,
    /// Well-formed lines this build cannot interpret — an unrecognized
    /// `record` tag, or a newer format version.
    ///
    /// Counted apart from `malformed_lines` so the user-facing warning stays
    /// truthful: a newer producer's records are not damage, and reporting
    /// them as such would send someone hunting a bug that does not exist.
    pub unknown_records: usize,
    /// Duplicates removed via `dedup_key`.
    pub deduped: usize,
}

impl ReadOutcome {
    /// File events only, in order. Turn markers are dropped.
    pub fn file_events(&self) -> impl Iterator<Item = &FileEvent> {
        self.records.iter().filter_map(LogRecord::as_file)
    }
}

/// Reads and merges every producer segment for one session.
pub fn read_session(paths: &EventPaths, session: Option<&str>) -> Result<ReadOutcome, LogError> {
    let dir = paths.session_dir(session);
    let mut outcome = ReadOutcome::default();

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(outcome),
        Err(e) => return Err(io_err(&dir)(e)),
    };

    let mut raw = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_err(&dir))?;
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let file = File::open(&path).map_err(io_err(&path))?;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(io_err(&path))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<LogRecord>(&line) {
                Ok(record) if record.is_understood() => raw.push(record),
                Ok(_) => outcome.unknown_records += 1,
                Err(_) => outcome.malformed_lines += 1,
            }
        }
    }

    let before = raw.len();
    let mut records = dedup(raw);
    // Stable ordering across producer segments read in arbitrary directory
    // order: timestamp first, then path, so equal-timestamp records do not
    // shuffle between runs. Turn markers have no path, so they tie-break
    // before any file event at the same timestamp.
    records.sort_by(|a, b| {
        a.ts().cmp(&b.ts()).then_with(|| {
            a.as_file()
                .map(|e| e.path.as_path())
                .cmp(&b.as_file().map(|e| e.path.as_path()))
        })
    });

    outcome.deduped = before - records.len();
    outcome.records = records;
    Ok(outcome)
}

/// Collapses records that report the same agent tool call.
///
/// A hook and a transcript tailer both observe the same `Read`. Because
/// Claude Code exposes `tool_use_id` on *both* paths (verified — see
/// `docs/hook-payload.md`), this is an exact set-membership test rather than
/// a timing heuristic. Push sources win ties: they observed the call directly.
/// Turn markers have no dedup key and pass straight through.
fn dedup(records: Vec<LogRecord>) -> Vec<LogRecord> {
    let mut chosen: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<Option<LogRecord>> = Vec::with_capacity(records.len());

    for record in records {
        let key = record.as_file().and_then(|e| e.dedup_key.clone());
        let Some(key) = key else {
            out.push(Some(record));
            continue;
        };
        match chosen.get(&key) {
            Some(&idx) => {
                let incumbent_is_push = out[idx]
                    .as_ref()
                    .and_then(LogRecord::as_file)
                    .is_some_and(|e| e.source.is_push());
                let candidate_is_push = record.as_file().is_some_and(|e| e.source.is_push());
                if candidate_is_push && !incumbent_is_push {
                    out[idx] = None;
                    chosen.insert(key, out.len());
                    out.push(Some(record));
                }
                // else: incumbent stands, duplicate discarded.
            }
            None => {
                chosen.insert(key, out.len());
                out.push(Some(record));
            }
        }
    }

    out.into_iter().flatten().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentId, EventKind, Source, Timestamp, TurnKind, TurnMarker};
    use crate::location::Workspace;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thirdeye-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn paths(tag: &str) -> EventPaths {
        EventPaths::with_state_root(tmpdir(tag), Workspace::new("/home/u/app"))
    }

    fn ev(ts: u64, path: &str, source: Source) -> FileEvent {
        FileEvent::new(
            Timestamp(ts),
            AgentId::Claude,
            path,
            EventKind::Read { range: None },
            source,
        )
    }

    #[test]
    fn round_trips_through_a_segment() {
        let paths = paths("roundtrip");
        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        w.append(&LogRecord::File(ev(2, "/b.rs", Source::Hook)))
            .unwrap();
        w.append(&LogRecord::File(ev(1, "/a.rs", Source::Hook)))
            .unwrap();

        let out = read_session(&paths, Some("s1")).unwrap();
        assert_eq!(out.records.len(), 2);
        assert_eq!(out.malformed_lines, 0);
        // Written out of order, read back in timestamp order.
        assert_eq!(out.records[0].ts(), Timestamp(1));
        assert_eq!(out.records[1].ts(), Timestamp(2));
    }

    #[cfg(windows)]
    #[test]
    fn one_file_named_in_two_cases_is_logged_as_one_path_on_windows() {
        // Measured: `claude -p` started in `c:\users\U\work\APP` reported
        // `C:\users\U\work\APP\notes.txt` under a root canonicalized to
        // `C:\Users\u\Work\app`, and `ls` drew it as a full path.
        let paths =
            EventPaths::with_state_root(tmpdir("respell"), Workspace::new(r"C:\Users\u\Work\app"));
        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        for (ts, path) in [
            (1, r"C:\users\U\work\APP\notes.txt"),
            (2, r"C:\Users\u\Work\app\notes.txt"),
        ] {
            w.append(&LogRecord::File(ev(ts, path, Source::Hook)))
                .unwrap();
        }
        let out = read_session(&paths, Some("s1")).unwrap();
        let logged: Vec<_> = out.file_events().map(|e| e.path.clone()).collect();
        let one = PathBuf::from(r"C:\Users\u\Work\app\notes.txt");
        assert_eq!(logged, [one.clone(), one]);
        assert_eq!(
            paths.workspace().relativize(&logged[0]),
            Path::new("notes.txt")
        );
    }

    #[test]
    fn merges_multiple_producer_segments() {
        let paths = paths("merge");
        Writer::open(&paths, Some("s1"), "hook")
            .unwrap()
            .append(&LogRecord::File(ev(10, "/a.rs", Source::Hook)))
            .unwrap();
        Writer::open(&paths, Some("s1"), "transcript")
            .unwrap()
            .append(&LogRecord::File(ev(5, "/b.rs", Source::Transcript)))
            .unwrap();

        let out = read_session(&paths, Some("s1")).unwrap();
        assert_eq!(out.records.len(), 2);
        assert_eq!(out.records[0].as_file().unwrap().path, Path::new("/b.rs"));
    }

    #[test]
    fn push_wins_over_pull_for_the_same_tool_call() {
        let paths = paths("dedup");
        // Transcript tailer sees it first; the hook's copy arrives later.
        Writer::open(&paths, Some("s1"), "transcript")
            .unwrap()
            .append(&LogRecord::File(
                ev(5, "/a.rs", Source::Transcript).with_dedup_key("toolu_1"),
            ))
            .unwrap();
        Writer::open(&paths, Some("s1"), "hook")
            .unwrap()
            .append(&LogRecord::File(
                ev(5, "/a.rs", Source::Hook).with_dedup_key("toolu_1"),
            ))
            .unwrap();

        let out = read_session(&paths, Some("s1")).unwrap();
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.deduped, 1);
        assert_eq!(out.records[0].as_file().unwrap().source, Source::Hook);
    }

    #[test]
    fn events_without_a_dedup_key_are_never_collapsed() {
        // Two genuine reads of the same file must both count toward ranking.
        let paths = paths("nokey");
        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        w.append(&LogRecord::File(ev(1, "/a.rs", Source::Hook)))
            .unwrap();
        w.append(&LogRecord::File(ev(2, "/a.rs", Source::Hook)))
            .unwrap();

        let out = read_session(&paths, Some("s1")).unwrap();
        assert_eq!(out.records.len(), 2);
        assert_eq!(out.deduped, 0);
    }

    #[test]
    fn torn_lines_are_counted_not_fatal() {
        let paths = paths("torn");
        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        w.append(&LogRecord::File(ev(1, "/a.rs", Source::Hook)))
            .unwrap();
        let seg = w.path().to_path_buf();
        drop(w);
        // Simulate a reader catching a producer mid-write.
        let mut f = OpenOptions::new().append(true).open(&seg).unwrap();
        f.write_all(b"{\"ts\":2,\"agent\":\"clau").unwrap();

        let out = read_session(&paths, Some("s1")).unwrap();
        assert_eq!(out.records.len(), 1);
        assert_eq!(out.malformed_lines, 1);
    }

    #[test]
    fn oversized_events_are_shrunk_rather_than_dropped() {
        let paths = paths("oversize");
        let mut ev = FileEvent::new(
            Timestamp(1),
            AgentId::Claude,
            "/a.rs",
            EventKind::Exec {
                cmd: "x".repeat(MAX_LINE_BYTES * 3),
            },
            Source::Hook,
        );
        ev.session = Some("s1".into());

        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        w.append(&LogRecord::File(ev)).unwrap();

        let written = std::fs::read_to_string(w.path()).unwrap();
        assert!(
            written.len() <= MAX_LINE_BYTES,
            "line was {} bytes",
            written.len()
        );
        let out = read_session(&paths, Some("s1")).unwrap();
        assert_eq!(out.records.len(), 1, "event must survive truncation");
    }

    /// The size cap is what makes concurrent `O_APPEND` writes atomic, so it
    /// is a property of *every* line, not of the one field a given test
    /// happens to stretch. Each case below stretches a different field —
    /// including `path`, which no truncation pass used to touch.
    #[test]
    fn every_written_line_stays_within_the_size_cap() {
        let long = "x".repeat(MAX_LINE_BYTES * 3);
        let cases: Vec<(&str, LogRecord)> = vec![
            (
                "pathological path",
                LogRecord::File(FileEvent::new(
                    Timestamp(1),
                    AgentId::Claude,
                    format!("/w/{long}"),
                    EventKind::Mention,
                    Source::Hook,
                )),
            ),
            (
                "long cmd",
                LogRecord::File(FileEvent::new(
                    Timestamp(2),
                    AgentId::Claude,
                    "/w/a.rs",
                    EventKind::Exec { cmd: long.clone() },
                    Source::Hook,
                )),
            ),
            (
                "long pattern on a long path",
                LogRecord::File(FileEvent::new(
                    Timestamp(3),
                    AgentId::Claude,
                    format!("/w/{long}"),
                    EventKind::Search {
                        pattern: long.clone(),
                    },
                    Source::Hook,
                )),
            ),
            (
                "long flag reason on a long path",
                LogRecord::File(FileEvent::new(
                    Timestamp(4),
                    AgentId::Claude,
                    format!("/w/{long}"),
                    EventKind::Flag {
                        reason: long.clone(),
                        range: None,
                    },
                    Source::Hook,
                )),
            ),
            (
                "long prompt",
                turn(
                    5,
                    TurnKind::Open {
                        prompt: long.clone(),
                    },
                ),
            ),
        ];

        let paths = paths("size-cap");
        for (name, record) in cases {
            let mut w = Writer::open(&paths, Some(name), "hook").unwrap();
            w.append(&record).unwrap_or_else(|e| panic!("{name}: {e}"));
            let written = std::fs::read_to_string(w.path()).unwrap();
            for line in written.lines() {
                assert!(
                    line.len() < MAX_LINE_BYTES,
                    "{name}: wrote a {}-byte line, {} over the {MAX_LINE_BYTES}-byte cap",
                    line.len() + 1,
                    line.len() + 1 - MAX_LINE_BYTES
                );
            }
            let out = read_session(&paths, Some(name)).unwrap();
            assert_eq!(out.records.len(), 1, "{name}: record must survive");
        }
    }

    /// Writes `lines` verbatim into a session segment, bypassing `Writer`,
    /// so a reader can be shown input this build would never produce.
    fn plant_lines(paths: &EventPaths, session: &str, lines: &[&str]) {
        let seg = paths.segment(Some(session), "future");
        std::fs::create_dir_all(seg.parent().unwrap()).unwrap();
        std::fs::write(&seg, lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn records_from_a_newer_producer_are_skipped_but_not_called_malformed() {
        // A stale binary reading a fresh log is routine: the plugin is
        // symlinked from a checkout while the binary is installed separately.
        // Reporting a newer record as corruption would send the user hunting
        // a bug that does not exist.
        let paths = paths("future");
        plant_lines(
            &paths,
            "s1",
            &[
                // A record type this build has never heard of.
                r#"{"record":"snapshot","ts":1,"blob":"whatever"}"#,
                // A known type, but from a newer format version.
                r#"{"record":"file","v":99,"ts":2,"agent":"claude","path":"/a.rs","kind":"read","range":null,"source":"hook"}"#,
                // Genuinely damaged.
                r#"{"record":"file","ts":"not-a-number"}"#,
            ],
        );
        Writer::open(&paths, Some("s1"), "hook")
            .unwrap()
            .append(&LogRecord::File(ev(3, "/b.rs", Source::Hook)))
            .unwrap();

        let out = read_session(&paths, Some("s1")).unwrap();
        assert_eq!(out.unknown_records, 2, "both future records must count");
        assert_eq!(out.malformed_lines, 1, "only the damaged line is damage");
        assert_eq!(out.records.len(), 1, "the readable record still arrives");
    }

    #[test]
    fn an_unrecognized_record_tag_deserializes_instead_of_erroring() {
        let record: LogRecord =
            serde_json::from_str(r#"{"record":"snapshot","ts":1}"#).expect("must not error");
        assert_eq!(record, LogRecord::Unknown);
        assert!(!record.is_understood());
        assert!(record.as_file().is_none());
    }

    #[test]
    fn a_record_that_cannot_be_shrunk_is_refused_rather_than_written() {
        // Reachable only when the *identity* fields are pathological, since
        // they are the ones truncation must not touch: clipping a dedup_key
        // would silently break dedup, which is worse than losing one record.
        // Writing it anyway would break append atomicity for every concurrent
        // producer, so refusing is the conservative end of the trade.
        let paths = paths("refuse");
        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        let ev = ev(1, "/a.rs", Source::Hook).with_dedup_key("k".repeat(MAX_LINE_BYTES * 2));

        let err = w.append(&LogRecord::File(ev)).unwrap_err();
        assert!(
            matches!(err, LogError::Oversized { .. }),
            "expected Oversized, got {err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(w.path()).unwrap(),
            "",
            "nothing may reach the segment once the cap cannot be met"
        );
    }

    #[cfg(unix)]
    #[test]
    fn state_is_readable_only_by_its_owner() {
        // The log carries full prompt text and every path the agent touched.
        // On a shared host the default umask makes both group- and
        // world-readable.
        use std::os::unix::fs::PermissionsExt;

        let paths = paths("perms");
        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        w.append(&LogRecord::File(ev(1, "/a.rs", Source::Hook)))
            .unwrap();

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(w.path()), 0o600, "segment is not owner-only");
        assert_eq!(
            mode(&paths.session_dir(Some("s1"))),
            0o700,
            "session dir is not owner-only"
        );
        assert_eq!(
            mode(&paths.workspace_dir()),
            0o700,
            "workspace dir is not owner-only"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_directory_from_an_earlier_build_is_tightened() {
        use std::os::unix::fs::PermissionsExt;

        let paths = paths("perms-upgrade");
        let dir = paths.session_dir(Some("s1"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let _ = Writer::open(&paths, Some("s1"), "hook").unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "an existing loose directory must be corrected");
    }

    #[test]
    fn missing_session_dir_reads_as_empty() {
        let out = read_session(&paths("absent"), Some("never-existed")).unwrap();
        assert!(out.records.is_empty());
    }

    fn turn(ts: u64, kind: TurnKind) -> LogRecord {
        LogRecord::Turn(TurnMarker::new(Timestamp(ts), "p1", kind).with_session("s1"))
    }

    #[test]
    fn turn_markers_sort_before_same_timestamp_file_events_and_survive_dedup() {
        let paths = paths("turns");
        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        // Two reports of the same tool call, at the same ts as a turn marker.
        w.append(&LogRecord::File(
            ev(5, "/a.rs", Source::Hook).with_dedup_key("toolu_1"),
        ))
        .unwrap();
        w.append(&turn(
            5,
            TurnKind::Open {
                prompt: "start".into(),
            },
        ))
        .unwrap();
        w.append(&LogRecord::File(
            ev(5, "/a.rs", Source::Transcript).with_dedup_key("toolu_1"),
        ))
        .unwrap();
        w.append(&turn(6, TurnKind::Close)).unwrap();

        let out = read_session(&paths, Some("s1")).unwrap();
        // The duplicate /a.rs report collapses; both turn markers pass
        // through untouched — they have no dedup key.
        assert_eq!(out.records.len(), 3);
        assert_eq!(out.deduped, 1);

        // Same-timestamp tie-break: the turn marker (no path) sorts before
        // the file event (Some(path)), per `None < Some(_)`.
        assert_eq!(out.records[0].ts(), Timestamp(5));
        assert!(
            matches!(out.records[0], LogRecord::Turn(_)),
            "turn marker must sort before the file event at the same ts"
        );
        assert_eq!(out.records[1].ts(), Timestamp(5));
        assert!(out.records[1].as_file().is_some());

        assert_eq!(out.records[2].ts(), Timestamp(6));
        assert!(matches!(out.records[2], LogRecord::Turn(_)));
    }

    #[test]
    fn oversized_turn_open_prompts_are_shrunk_rather_than_dropped() {
        let paths = paths("turn-oversize");
        let marker = turn(
            1,
            TurnKind::Open {
                prompt: "x".repeat(MAX_LINE_BYTES * 3),
            },
        );

        let mut w = Writer::open(&paths, Some("s1"), "hook").unwrap();
        w.append(&marker).unwrap();

        let written = std::fs::read_to_string(w.path()).unwrap();
        assert!(
            written.len() <= MAX_LINE_BYTES,
            "line was {} bytes",
            written.len()
        );

        let out = read_session(&paths, Some("s1")).unwrap();
        assert_eq!(out.records.len(), 1, "turn marker must survive truncation");
        assert!(
            matches!(&out.records[0], LogRecord::Turn(_)),
            "truncated record must still be a turn marker"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_filesystem_without_permission_bits_does_not_silence_the_log() {
        // The failure this guards is invisible by construction: the hook
        // wrapper swallows a failing `thirdeye hook` into hook-errors.log, so
        // a fatal chmod would show up only as an `ls` that never fills in.
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::Unsupported,
        ] {
            assert!(
                permissions_unsupported(&std::io::Error::from(kind)),
                "{kind:?} must be treated as 'no permission bits here', not as failure"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_genuine_chmod_failure_is_still_reported() {
        // The tolerance above must stay narrow: blanket-ignoring every chmod
        // error would hide real I/O trouble on a filesystem that does support
        // modes, which is the opposite mistake.
        assert!(!permissions_unsupported(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));

        let missing = tmpdir("chmod-missing").join("no-such-file");
        let err = restrict(&missing, 0o600).expect_err("chmod on a missing path must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
