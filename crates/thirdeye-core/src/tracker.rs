//! The ranked set of files currently in play for **one** session.
//!
//! `TrackerState` is deliberately per-session: it keeps the "which session is
//! this head attached to" question entirely inside heads, and out of the core.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::event::{AgentId, EventKind, FileEvent, LineRange, Timestamp};
use crate::location::Workspace;

/// Time for an event's ranking contribution to lose half its weight.
pub const DEFAULT_HALF_LIFE_MS: u64 = 5 * 60 * 1000;

/// Tolerance when deciding whether a file changed *after* the agent saw it.
///
/// The agent's own `Edit` writes the file, so its mtime naturally lands a
/// moment after the event timestamp. Without a grace window every edit would
/// immediately flag itself as modified-behind-the-agent's-back.
pub const STALENESS_GRACE_MS: u64 = 2_000;

/// Whether a tracked file still matches what the agent last saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staleness {
    /// No content-bearing event yet, or the file could not be stat'd.
    Unknown,
    /// Unchanged since the agent last read or wrote it.
    Fresh,
    /// Changed underneath the agent — the interesting case.
    Modified,
}

/// Per-kind observation counts, for UI badges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KindCounts {
    pub read: u32,
    pub edit: u32,
    pub write: u32,
    pub mention: u32,
    pub search: u32,
    pub exec: u32,
    pub flag: u32,
    pub delete: u32,
}

impl KindCounts {
    /// Takes the [`EventKind`] rather than its label so that adding a variant
    /// is a compile error here. Dispatching on `&str` silently dropped
    /// `Flag` into a catch-all: the event contributed its weight to the
    /// ranking score but incremented no counter, so `total()` under-reported.
    fn bump(&mut self, kind: &EventKind) {
        match kind {
            EventKind::Read { .. } => self.read += 1,
            EventKind::Edit { .. } => self.edit += 1,
            EventKind::Write => self.write += 1,
            EventKind::Mention => self.mention += 1,
            EventKind::Search { .. } => self.search += 1,
            EventKind::Exec { .. } => self.exec += 1,
            EventKind::Flag { .. } => self.flag += 1,
            EventKind::Delete => self.delete += 1,
        }
    }

    pub fn total(&self) -> u32 {
        self.read
            + self.edit
            + self.write
            + self.mention
            + self.search
            + self.exec
            + self.flag
            + self.delete
    }

    /// Whether the agent changed the file, as opposed to only looking at it.
    pub fn mutated(&self) -> bool {
        self.edit > 0 || self.write > 0 || self.delete > 0
    }
}

/// One file the agent has touched.
#[derive(Debug, Clone)]
pub struct TrackedFile {
    pub path: PathBuf,
    pub first_seen: Timestamp,
    pub last_seen: Timestamp,
    pub counts: KindCounts,
    /// The line range of the most recent ranged event — what the TUI scrolls
    /// to, so the user lands where the agent was working rather than line 1.
    pub last_range: Option<LineRange>,
    /// Timestamp of the last event implying the agent holds this content.
    pub last_content_seen: Option<Timestamp>,
    pub agents: Vec<AgentId>,
    /// User-pinned files sort above everything regardless of score.
    pub pinned: bool,
    /// Decayed score, valid as of `score_ts`.
    score: f64,
    score_ts: Timestamp,
}

impl TrackedFile {
    fn new(path: PathBuf, ts: Timestamp) -> Self {
        Self {
            path,
            first_seen: ts,
            last_seen: ts,
            counts: KindCounts::default(),
            last_range: None,
            last_content_seen: None,
            agents: Vec::new(),
            pinned: false,
            score: 0.0,
            score_ts: ts,
        }
    }

    /// Ranking score decayed to `now`.
    ///
    /// Because decay is exponential, carrying a single running total and
    /// decaying it forward is exactly equal to summing every event's
    /// individually-decayed weight — so this stays O(1) in memory per file
    /// instead of retaining the whole event history.
    pub fn score_at(&self, now: Timestamp, half_life_ms: u64) -> f64 {
        self.score * decay(self.score_ts.elapsed_to(now), half_life_ms)
    }

    /// Staleness given the file's current mtime. Pure, so the transition can
    /// be tested without touching a filesystem clock.
    pub fn staleness_with_mtime(&self, mtime: Option<SystemTime>) -> Staleness {
        let (Some(seen), Some(mtime)) = (self.last_content_seen, mtime) else {
            return Staleness::Unknown;
        };
        let mtime = Timestamp::from_system_time(mtime);
        if mtime.0 > seen.0.saturating_add(STALENESS_GRACE_MS) {
            Staleness::Modified
        } else {
            Staleness::Fresh
        }
    }

    /// Staleness by stat'ing the file. An unreadable file reads as
    /// [`Staleness::Unknown`] rather than as fresh — absence of evidence must
    /// not render as a clean bill of health.
    pub fn staleness(&self) -> Staleness {
        let mtime = std::fs::metadata(&self.path)
            .and_then(|m| m.modified())
            .ok();
        self.staleness_with_mtime(mtime)
    }
}

fn decay(age_ms: u64, half_life_ms: u64) -> f64 {
    if half_life_ms == 0 {
        return 1.0;
    }
    0.5f64.powf(age_ms as f64 / half_life_ms as f64)
}

/// The live file set for one session.
#[derive(Debug, Clone)]
pub struct TrackerState {
    workspace: Workspace,
    session: Option<String>,
    files: HashMap<PathBuf, TrackedFile>,
    half_life_ms: u64,
    latest_event: Option<Timestamp>,
}

impl TrackerState {
    pub fn new(workspace: Workspace, session: Option<String>) -> Self {
        Self {
            workspace,
            session,
            files: HashMap::new(),
            half_life_ms: DEFAULT_HALF_LIFE_MS,
            latest_event: None,
        }
    }

    pub fn with_half_life_ms(mut self, half_life_ms: u64) -> Self {
        self.half_life_ms = half_life_ms;
        self
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Timestamp of the most recent event applied — the head's "is this
    /// session still live" signal.
    pub fn latest_event(&self) -> Option<Timestamp> {
        self.latest_event
    }

    pub fn get(&self, path: &Path) -> Option<&TrackedFile> {
        self.files.get(path)
    }

    pub fn apply(&mut self, event: &FileEvent) {
        let entry = self
            .files
            .entry(event.path.clone())
            .or_insert_with(|| TrackedFile::new(event.path.clone(), event.ts));

        // Decay the running total to this event's time, then add its weight.
        entry.score = entry.score * decay(entry.score_ts.elapsed_to(event.ts), self.half_life_ms)
            + event.kind.weight();
        entry.score_ts = entry.score_ts.max(event.ts);

        entry.first_seen = entry.first_seen.min(event.ts);
        entry.last_seen = entry.last_seen.max(event.ts);
        entry.counts.bump(&event.kind);

        if let Some(range) = event.kind.range() {
            entry.last_range = Some(range);
        }
        if event.kind.observes_content() {
            entry.last_content_seen = Some(
                entry
                    .last_content_seen
                    .map_or(event.ts, |p| p.max(event.ts)),
            );
        }
        if !entry.agents.contains(&event.agent) {
            entry.agents.push(event.agent.clone());
        }

        self.latest_event = Some(self.latest_event.map_or(event.ts, |p| p.max(event.ts)));
    }

    pub fn apply_all<'e>(&mut self, events: impl IntoIterator<Item = &'e FileEvent>) {
        for event in events {
            self.apply(event);
        }
    }

    pub fn set_pinned(&mut self, path: &Path, pinned: bool) -> bool {
        match self.files.get_mut(path) {
            Some(f) => {
                f.pinned = pinned;
                true
            }
            None => false,
        }
    }

    /// Files ordered as the UI should show them: pinned first, then by
    /// decayed score, with path as a deterministic tie-break.
    pub fn ranked(&self, now: Timestamp) -> Vec<&TrackedFile> {
        let mut files: Vec<&TrackedFile> = self.files.values().collect();
        files.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| {
                    b.score_at(now, self.half_life_ms)
                        .total_cmp(&a.score_at(now, self.half_life_ms))
                })
                .then_with(|| a.path.cmp(&b.path))
        });
        files
    }

    /// Display path for a tracked file, relative to the workspace when possible.
    pub fn display_path<'p>(&self, file: &'p TrackedFile) -> &'p Path {
        self.workspace.relativize(&file.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventKind, Source};
    use std::time::Duration;

    const MIN: u64 = 60_000;

    fn state() -> TrackerState {
        TrackerState::new(Workspace::new("/home/u/app"), Some("s1".into()))
    }

    fn ev(ts: u64, path: &str, kind: EventKind) -> FileEvent {
        FileEvent::new(Timestamp(ts), AgentId::Claude, path, kind, Source::Hook)
    }

    fn read(ts: u64, path: &str) -> FileEvent {
        ev(ts, path, EventKind::Read { range: None })
    }

    fn edit(ts: u64, path: &str) -> FileEvent {
        ev(ts, path, EventKind::Edit { range: None })
    }

    #[test]
    fn edited_files_outrank_read_files_seen_at_the_same_time() {
        let mut s = state();
        s.apply(&read(0, "/home/u/app/read.rs"));
        s.apply(&edit(0, "/home/u/app/edit.rs"));

        let ranked = s.ranked(Timestamp(0));
        assert_eq!(ranked[0].path, Path::new("/home/u/app/edit.rs"));
    }

    #[test]
    fn recent_activity_outranks_a_larger_but_older_pile() {
        let mut s = state();
        // Five reads long ago...
        for i in 0..5 {
            s.apply(&read(i, "/old.rs"));
        }
        // ...versus one read just now, a half-life-heavy gap later.
        let now = Timestamp(30 * MIN);
        s.apply(&read(now.0, "/new.rs"));

        let ranked = s.ranked(now);
        assert_eq!(
            ranked[0].path,
            Path::new("/new.rs"),
            "decay failed to demote the stale pile"
        );
    }

    #[test]
    fn repeated_access_accumulates_within_a_half_life() {
        let mut s = state();
        s.apply(&read(0, "/once.rs"));
        for i in 0..4 {
            s.apply(&read(i * 100, "/often.rs"));
        }

        let ranked = s.ranked(Timestamp(500));
        assert_eq!(ranked[0].path, Path::new("/often.rs"));
    }

    #[test]
    fn decaying_a_running_total_equals_summing_decayed_weights() {
        // The O(1) score is only legitimate if it matches the naive sum.
        let mut s = state().with_half_life_ms(MIN);
        let stamps = [0u64, 10_000, 45_000, 90_000];
        for t in stamps {
            s.apply(&read(t, "/a.rs"));
        }

        let now = Timestamp(120_000);
        let naive: f64 = stamps
            .iter()
            .map(|t| 3.0 * decay(Timestamp(*t).elapsed_to(now), MIN))
            .sum();
        let actual = s.get(Path::new("/a.rs")).unwrap().score_at(now, MIN);

        assert!(
            (naive - actual).abs() < 1e-9,
            "running total {actual} diverged from naive sum {naive}"
        );
    }

    #[test]
    fn pinned_files_outrank_everything() {
        let mut s = state();
        s.apply(&read(0, "/pinned.rs"));
        for _ in 0..10 {
            s.apply(&edit(1000, "/hot.rs"));
        }
        assert!(s.set_pinned(Path::new("/pinned.rs"), true));

        assert_eq!(s.ranked(Timestamp(1000))[0].path, Path::new("/pinned.rs"));
    }

    #[test]
    fn last_range_tracks_the_most_recent_ranged_event() {
        let mut s = state();
        s.apply(&ev(
            1,
            "/a.rs",
            EventKind::Read {
                range: Some(LineRange::new(1, 20)),
            },
        ));
        s.apply(&ev(
            2,
            "/a.rs",
            EventKind::Edit {
                range: Some(LineRange::new(80, 95)),
            },
        ));
        // A rangeless event must not erase the known location.
        s.apply(&ev(3, "/a.rs", EventKind::Write));

        assert_eq!(
            s.get(Path::new("/a.rs")).unwrap().last_range,
            Some(LineRange::new(80, 95))
        );
    }

    #[test]
    fn out_of_order_events_do_not_corrupt_bounds() {
        let mut s = state();
        s.apply(&read(5000, "/a.rs"));
        s.apply(&read(1000, "/a.rs"));

        let f = s.get(Path::new("/a.rs")).unwrap();
        assert_eq!(f.first_seen, Timestamp(1000));
        assert_eq!(f.last_seen, Timestamp(5000));
        assert!(f.score_at(Timestamp(5000), DEFAULT_HALF_LIFE_MS) > 0.0);
    }

    // --- staleness: the badge must be shown to go red, not just green ---

    fn tracked_with_content_seen_at(ts: u64) -> TrackedFile {
        let mut f = TrackedFile::new(PathBuf::from("/a.rs"), Timestamp(ts));
        f.last_content_seen = Some(Timestamp(ts));
        f
    }

    #[test]
    fn staleness_is_fresh_when_the_file_predates_the_read() {
        let f = tracked_with_content_seen_at(100_000);
        let mtime = Timestamp(90_000).to_system_time();
        assert_eq!(f.staleness_with_mtime(Some(mtime)), Staleness::Fresh);
    }

    #[test]
    fn staleness_flips_to_modified_when_the_file_changes_afterwards() {
        let f = tracked_with_content_seen_at(100_000);
        let mtime = Timestamp(100_000 + STALENESS_GRACE_MS + 1).to_system_time();
        assert_eq!(f.staleness_with_mtime(Some(mtime)), Staleness::Modified);
    }

    #[test]
    fn an_agents_own_write_does_not_flag_itself_as_modified() {
        let f = tracked_with_content_seen_at(100_000);
        let mtime = Timestamp(100_000 + STALENESS_GRACE_MS - 1).to_system_time();
        assert_eq!(f.staleness_with_mtime(Some(mtime)), Staleness::Fresh);
    }

    #[test]
    fn staleness_is_unknown_without_evidence() {
        let never_read = TrackedFile::new(PathBuf::from("/a.rs"), Timestamp(0));
        assert_eq!(
            never_read.staleness_with_mtime(Some(SystemTime::now())),
            Staleness::Unknown,
            "a file only mentioned has no baseline to compare against"
        );

        let unstattable = tracked_with_content_seen_at(1);
        assert_eq!(
            unstattable.staleness_with_mtime(None),
            Staleness::Unknown,
            "an unreadable file must not read as Fresh"
        );
    }

    #[test]
    fn staleness_detects_a_real_on_disk_change() {
        // Perturbation test: prove the badge fires against a real filesystem,
        // not only against synthesized timestamps.
        let path = std::env::temp_dir().join(format!("thirdeye-stale-{}.txt", std::process::id()));
        std::fs::write(&path, "before").unwrap();

        let mut f = TrackedFile::new(path.clone(), Timestamp(0));
        f.last_content_seen = Some(Timestamp::now());
        assert_eq!(f.staleness(), Staleness::Fresh);

        std::thread::sleep(Duration::from_millis(STALENESS_GRACE_MS + 50));
        std::fs::write(&path, "after").unwrap();
        assert_eq!(f.staleness(), Staleness::Modified);

        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            f.staleness(),
            Staleness::Unknown,
            "a deleted file is not 'fresh'"
        );
    }

    #[test]
    fn counts_and_agents_are_recorded_without_duplication() {
        let mut s = state();
        s.apply(&read(1, "/a.rs"));
        s.apply(&read(2, "/a.rs"));
        s.apply(&edit(3, "/a.rs"));
        let mut from_codex = read(4, "/a.rs");
        from_codex.agent = AgentId::Codex;
        s.apply(&from_codex);

        let f = s.get(Path::new("/a.rs")).unwrap();
        assert_eq!(f.counts.read, 3);
        assert_eq!(f.counts.edit, 1);
        assert_eq!(f.counts.total(), 4);
        assert!(f.counts.mutated());
        assert_eq!(f.agents, vec![AgentId::Claude, AgentId::Codex]);
    }

    #[test]
    fn every_event_kind_lands_in_a_counter() {
        // `total()` is the file's observation count; a kind that increments
        // nothing makes it silently under-report. This used to happen to
        // `Flag`, which fell into a `&str` match's catch-all while still
        // contributing its weight to the ranking score.
        let kinds = [
            EventKind::Read { range: None },
            EventKind::Edit { range: None },
            EventKind::Write,
            EventKind::Mention,
            EventKind::Search {
                pattern: "x".into(),
            },
            EventKind::Exec { cmd: "ls".into() },
            EventKind::Flag {
                reason: "look here".into(),
                range: None,
            },
        ];

        for (i, kind) in kinds.iter().enumerate() {
            let mut s = state();
            let ev = FileEvent::new(
                Timestamp(i as u64 + 1),
                AgentId::Claude,
                "/a.rs",
                kind.clone(),
                Source::Hook,
            );
            s.apply(&ev);
            assert_eq!(
                s.get(Path::new("/a.rs")).unwrap().counts.total(),
                1,
                "{} incremented no counter",
                kind.label()
            );
        }

        // And they accumulate together rather than shadowing each other.
        let mut s = state();
        for (i, kind) in kinds.iter().enumerate() {
            s.apply(&FileEvent::new(
                Timestamp(i as u64 + 1),
                AgentId::Claude,
                "/a.rs",
                kind.clone(),
                Source::Hook,
            ));
        }
        let counts = &s.get(Path::new("/a.rs")).unwrap().counts;
        assert_eq!(counts.total(), kinds.len() as u32);
        assert_eq!(counts.flag, 1);
    }

    #[test]
    fn display_path_is_workspace_relative() {
        let mut s = state();
        s.apply(&read(1, "/home/u/app/src/main.rs"));
        let f = s.get(Path::new("/home/u/app/src/main.rs")).unwrap();
        assert_eq!(s.display_path(f), Path::new("src/main.rs"));
    }

    #[test]
    fn deletions_are_counted() {
        let mut c = KindCounts::default();
        c.bump(&EventKind::Delete);
        assert_eq!(c.delete, 1);
        assert_eq!(c.total(), 1);
    }
}
