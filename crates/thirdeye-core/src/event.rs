//! The normalized event every adapter produces.
//!
//! This is the whole contract between adapters and heads: adapters translate
//! an agent-specific transport into [`FileEvent`], heads consume only
//! [`FileEvent`]. No head ever parses an agent format.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Version stamped on every log line this build writes.
///
/// Bumped when the *meaning* of an existing field changes — a new optional
/// field does not need it, since an old reader ignores what it does not know.
/// A reader seeing a higher version skips the record and says so rather than
/// misinterpreting it.
pub const LOG_FORMAT_VERSION: u32 = 1;

/// Lines written before the field existed are version 1 by definition.
fn default_format_version() -> u32 {
    1
}

/// Milliseconds since the Unix epoch.
///
/// Events cross process boundaries through a JSON log, so the wire format is
/// pinned to an integer rather than to `SystemTime`'s serde representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// Clock reads are the caller's job; this keeps the type testable.
    pub fn from_system_time(t: SystemTime) -> Self {
        Self(
            t.duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_millis() as u64,
        )
    }

    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    pub fn to_system_time(self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.0)
    }

    /// Milliseconds from `self` to `later`, saturating at zero.
    ///
    /// Saturating rather than signed: producers on different clocks can emit
    /// slightly out-of-order events, and a negative age would invert the
    /// decay in [`crate::tracker`].
    pub fn elapsed_to(self, later: Timestamp) -> u64 {
        later.0.saturating_sub(self.0)
    }
}

/// Which agent produced the event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentId {
    Claude,
    Codex,
    /// Spelled as the product spells itself; snake_case would make it
    /// `open_code`.
    #[serde(rename = "opencode")]
    OpenCode,
    Pi,
    Copilot,
    Antigravity,
    Grok,
    Cursor,
    /// An agent thirdeye has no dedicated adapter for.
    Generic(String),
}

/// How the event reached thirdeye.
///
/// [`Source::is_push`] drives the dedup precedence rule in
/// [`crate::tracker`]: push sources are authoritative, pull sources are the
/// retroactive fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Agent hook shelled out to `thirdeye emit`.
    Hook,
    /// Agent called the thirdeye MCP server.
    Mcp,
    /// thirdeye tailed a session transcript.
    Transcript,
    /// thirdeye polled an agent's state database.
    Sqlite,
}

impl Source {
    /// Push sources observe the agent as it acts; pull sources reconstruct
    /// after the fact and may replay events a push source already reported.
    pub fn is_push(self) -> bool {
        matches!(self, Source::Hook | Source::Mcp)
    }
}

/// A 1-based, end-exclusive line range.
///
/// Deliberately not `std::ops::Range<u32>`: this crosses a JSON boundary and
/// benefits from named fields and `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn new(start: u32, end: u32) -> Self {
        Self {
            start,
            end: end.max(start),
        }
    }

    pub fn len(self) -> u32 {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// What the agent did to the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    Read {
        range: Option<LineRange>,
    },
    Edit {
        range: Option<LineRange>,
    },
    Write,
    /// `@`-referenced in a user prompt.
    Mention,
    Search {
        pattern: String,
    },
    /// A shell command that named the path.
    Exec {
        cmd: String,
    },
    /// The agent explicitly asked the human to look at this file.
    Flag {
        reason: String,
        range: Option<LineRange>,
    },
    /// The file is gone. Seen across a shell call, since no agent's
    /// structured tools delete.
    Delete,
}

impl EventKind {
    /// Ranking weight. Mutation outranks reading, which outranks a mere
    /// reference — the list should surface what the agent is *working on*.
    pub fn weight(&self) -> f64 {
        match self {
            EventKind::Flag { .. } => 8.0,
            EventKind::Edit { .. } | EventKind::Write | EventKind::Delete => 5.0,
            EventKind::Read { .. } => 3.0,
            EventKind::Mention => 2.0,
            EventKind::Search { .. } | EventKind::Exec { .. } => 1.0,
        }
    }

    /// Whether this event implies the agent now holds the file's contents.
    /// Only these establish a baseline for staleness.
    pub fn observes_content(&self) -> bool {
        matches!(
            self,
            EventKind::Read { .. } | EventKind::Edit { .. } | EventKind::Write
        )
    }

    /// Stable discriminant, for per-kind counters and UI badges.
    pub fn label(&self) -> &'static str {
        match self {
            EventKind::Read { .. } => "read",
            EventKind::Edit { .. } => "edit",
            EventKind::Write => "write",
            EventKind::Mention => "mention",
            EventKind::Search { .. } => "search",
            EventKind::Exec { .. } => "exec",
            EventKind::Flag { .. } => "flag",
            EventKind::Delete => "delete",
        }
    }

    /// Whether the agent changed the file — what opens the view (§10.2) and
    /// lifts a hide. One place, so a new kind of change cannot be ranked as
    /// one and ignored as one elsewhere.
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            EventKind::Edit { .. } | EventKind::Write | EventKind::Delete
        )
    }

    pub fn range(&self) -> Option<LineRange> {
        match self {
            EventKind::Read { range } | EventKind::Edit { range } => *range,
            EventKind::Flag { range, .. } => *range,
            _ => None,
        }
    }
}

/// One observation of one file by one agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEvent {
    /// Log format version — see [`LOG_FORMAT_VERSION`].
    #[serde(default = "default_format_version")]
    pub v: u32,
    pub ts: Timestamp,
    pub agent: AgentId,
    /// `None` only for pull adapters that cannot attribute the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub path: PathBuf,
    #[serde(flatten)]
    pub kind: EventKind,
    pub source: Source,
    /// The agent's own id for the tool call (Claude Code's `tool_use_id`).
    ///
    /// Verified present on Claude Code hooks *and* transcript entries, so
    /// hook/transcript double-reporting dedups exactly rather than by
    /// heuristic. See `docs/hook-payload.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    /// Groups tool calls under the user prompt that caused them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
}

impl FileEvent {
    pub fn new(
        ts: Timestamp,
        agent: AgentId,
        path: impl Into<PathBuf>,
        kind: EventKind,
        source: Source,
    ) -> Self {
        Self {
            v: LOG_FORMAT_VERSION,
            ts,
            agent,
            session: None,
            path: path.into(),
            kind,
            source,
            dedup_key: None,
            prompt_id: None,
        }
    }

    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    pub fn with_dedup_key(mut self, key: impl Into<String>) -> Self {
        self.dedup_key = Some(key.into());
        self
    }

    pub fn with_prompt_id(mut self, id: impl Into<String>) -> Self {
        self.prompt_id = Some(id.into());
        self
    }
}

/// A conversational turn boundary. Has no path, so it is not a `FileEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMarker {
    /// Log format version — see [`LOG_FORMAT_VERSION`].
    #[serde(default = "default_format_version")]
    pub v: u32,
    pub ts: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Claude Code's `prompt_id`. Turn grouping depends entirely on this.
    pub prompt_id: String,
    #[serde(flatten)]
    pub kind: TurnKind,
}

impl TurnMarker {
    pub fn new(ts: Timestamp, prompt_id: impl Into<String>, kind: TurnKind) -> Self {
        Self {
            v: LOG_FORMAT_VERSION,
            ts,
            session: None,
            prompt_id: prompt_id.into(),
            kind,
        }
    }

    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "turn", rename_all = "snake_case")]
pub enum TurnKind {
    Open { prompt: String },
    Close,
}

/// One line of the event log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum LogRecord {
    File(FileEvent),
    Turn(TurnMarker),
    /// A record type this build does not know.
    ///
    /// `docs/install.md` symlinks `plugin/` from a checkout while the binary
    /// is installed separately, so a stale binary reading a log a newer one
    /// wrote is a *normal* state, not corruption. Without this variant such a
    /// line fails to parse and is counted as malformed, which `thirdeye ls`
    /// reports to the user as damage. The payload is discarded — there is
    /// nothing this build could do with it — but the line is accounted for
    /// honestly.
    ///
    /// A unit variant because `#[serde(other)]` accepts nothing else inside
    /// an internally tagged enum.
    #[serde(other)]
    Unknown,
}

impl LogRecord {
    /// Timestamp of an [`Unknown`](LogRecord::Unknown) record is zero: its
    /// fields were discarded. Readers filter unknown records out before
    /// ordering, so this is never used to sort.
    pub fn ts(&self) -> Timestamp {
        match self {
            LogRecord::File(e) => e.ts,
            LogRecord::Turn(t) => t.ts,
            LogRecord::Unknown => Timestamp(0),
        }
    }

    /// The format version the producer stamped on this record.
    pub fn format_version(&self) -> u32 {
        match self {
            LogRecord::File(e) => e.v,
            LogRecord::Turn(t) => t.v,
            // An unknown tag is by definition not a version this build can
            // interpret, whatever number it carries.
            LogRecord::Unknown => u32::MAX,
        }
    }

    /// Whether this build can interpret the record.
    pub fn is_understood(&self) -> bool {
        self.format_version() <= LOG_FORMAT_VERSION
    }

    pub fn as_file(&self) -> Option<&FileEvent> {
        match self {
            LogRecord::File(e) => Some(e),
            LogRecord::Turn(_) | LogRecord::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_through_json() {
        let ev = FileEvent::new(
            Timestamp(1_700_000_000_000),
            AgentId::Claude,
            "/src/main.rs",
            EventKind::Read {
                range: Some(LineRange::new(10, 40)),
            },
            Source::Hook,
        )
        .with_session("sess-1")
        .with_dedup_key("toolu_abc")
        .with_prompt_id("prompt-1");

        let line = serde_json::to_string(&ev).unwrap();
        assert!(!line.contains('\n'), "log lines must stay single-line");
        assert_eq!(serde_json::from_str::<FileEvent>(&line).unwrap(), ev);
    }

    #[test]
    fn optional_fields_are_omitted_when_absent() {
        let ev = FileEvent::new(
            Timestamp(1),
            AgentId::Codex,
            "/a.rs",
            EventKind::Write,
            Source::Sqlite,
        );
        let line = serde_json::to_string(&ev).unwrap();
        assert!(!line.contains("session"));
        assert!(!line.contains("dedup_key"));
    }

    #[test]
    fn mutation_outranks_reading_outranks_reference() {
        assert!(EventKind::Write.weight() > EventKind::Read { range: None }.weight());
        assert!(EventKind::Read { range: None }.weight() > EventKind::Mention.weight());
    }

    #[test]
    fn elapsed_saturates_on_out_of_order_clocks() {
        assert_eq!(Timestamp(500).elapsed_to(Timestamp(100)), 0);
        assert_eq!(Timestamp(100).elapsed_to(Timestamp(500)), 400);
    }

    #[test]
    fn only_content_bearing_kinds_establish_a_staleness_baseline() {
        assert!(EventKind::Read { range: None }.observes_content());
        assert!(EventKind::Write.observes_content());
        assert!(!EventKind::Mention.observes_content());
        assert!(
            !EventKind::Search {
                pattern: "x".into()
            }
            .observes_content()
        );
    }

    #[test]
    fn push_and_pull_sources_are_classified() {
        assert!(Source::Hook.is_push());
        assert!(Source::Mcp.is_push());
        assert!(!Source::Transcript.is_push());
        assert!(!Source::Sqlite.is_push());
    }

    #[test]
    fn a_line_written_before_the_version_field_existed_still_reads() {
        // The field is `default`ed rather than required so the escape hatch
        // does not itself become a breaking change.
        let line = r#"{"record":"file","ts":7,"agent":"claude","path":"/a.rs",
                       "kind":"write","source":"hook"}"#;
        let record: LogRecord = serde_json::from_str(line).unwrap();
        assert_eq!(record.format_version(), 1);
        assert!(record.is_understood());
        assert_eq!(record.as_file().unwrap().path, PathBuf::from("/a.rs"));
    }

    #[test]
    fn every_record_this_build_writes_carries_the_current_version() {
        let file = LogRecord::File(FileEvent::new(
            Timestamp(1),
            AgentId::Claude,
            "/a.rs",
            EventKind::Write,
            Source::Hook,
        ));
        let turn = LogRecord::Turn(TurnMarker::new(Timestamp(1), "p1", TurnKind::Close));
        for record in [file, turn] {
            assert_eq!(record.format_version(), LOG_FORMAT_VERSION);
            assert!(
                serde_json::to_string(&record).unwrap().contains("\"v\":"),
                "the version must be on the wire, not only in memory"
            );
        }
    }

    #[test]
    fn log_records_round_trip_through_json() {
        let file = LogRecord::File(FileEvent::new(
            Timestamp(1_700_000_000_000),
            AgentId::Claude,
            "/src/main.rs",
            EventKind::Flag {
                reason: "dedup tie-break looks wrong".into(),
                range: Some(LineRange::new(88, 96)),
            },
            Source::Mcp,
        ));
        let open = LogRecord::Turn(
            TurnMarker::new(
                Timestamp(1_700_000_000_001),
                "p1",
                TurnKind::Open {
                    prompt: "fix dedup".into(),
                },
            )
            .with_session("s1"),
        );
        let close = LogRecord::Turn(
            TurnMarker::new(Timestamp(1_700_000_000_002), "p1", TurnKind::Close).with_session("s1"),
        );

        for record in [file, open, close] {
            let line = serde_json::to_string(&record).unwrap();
            assert!(!line.contains('\n'), "log lines must stay single-line");
            assert_eq!(
                serde_json::from_str::<LogRecord>(&line).unwrap(),
                record,
                "round-trip changed the record: {line}"
            );
        }
    }

    #[test]
    fn a_deletion_is_a_change_with_a_name_of_its_own() {
        // Calling a removed file a write would be a lie the human acts on.
        let gone = EventKind::Delete;
        assert_eq!(gone.label(), "delete");
        assert!(gone.is_mutation());
        assert_eq!(gone.weight(), EventKind::Write.weight());
        assert!(
            !gone.observes_content(),
            "a gone file's content is not held"
        );
        assert_eq!(
            serde_json::to_string(&gone).unwrap(),
            r#"{"kind":"delete"}"#
        );
        assert!(EventKind::Write.is_mutation());
        assert!(!EventKind::Read { range: None }.is_mutation());
    }
}
