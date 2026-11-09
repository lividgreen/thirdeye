# thirdeye Capture Pipeline Implementation Plan

**Goal:** A Claude Code plugin whose hooks record every file the agent touches into a session-keyed event log, grouped by conversational turn, inspectable with `thirdeye ls`.

**Architecture:** Claude Code fires hooks → `thirdeye hook` reads the JSON payload on stdin, translates it into `LogRecord`s, and appends them to a per-session log segment. Nothing else runs. The daemon, head and surface arrive in plan 2; this plan proves the capture half end-to-end.

**Tech Stack:** Rust 2024 (1.94), serde/serde_json, clap 4. No async runtime yet.

## Global Constraints

- Rust edition 2024, `rust-version = "1.94"`. New crates must declare `[lints] workspace = true`.
- `unsafe_code = "forbid"`, `clippy::all = "deny"` at workspace level — lint failures are build failures.
- All four gates pass before every commit: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, `cargo build --workspace`.
- **Rootless:** no writes outside `$HOME` and `$XDG_RUNTIME_DIR`. No `sudo`, no setuid, no system-wide paths.
- **XDG:** state under `$XDG_STATE_HOME/thirdeye/`, runtime under `$XDG_RUNTIME_DIR/thirdeye/`.
- Tests are colocated in `#[cfg(test)] mod tests` at the bottom of each module.
- Test names state the property held, not the function called.
- Log lines stay under `MAX_LINE_BYTES` (4096).

## Already built (do not rewrite)

`crates/thirdeye-core` is implemented and green — 35 tests. Available API:

```rust
// event.rs
pub struct Timestamp(pub u64);            // ms since epoch; ::now(), ::from_system_time, .elapsed_to
pub enum AgentId { Claude, Codex, Cursor, Generic(String) }
pub enum Source  { Hook, Mcp, Transcript, Sqlite }   // .is_push()
pub struct LineRange { pub start: u32, pub end: u32 }  // ::new(start,end), .len(), .is_empty()
pub enum EventKind { Read{range:Option<LineRange>}, Edit{range:Option<LineRange>}, Write,
                     Mention, Search{pattern:String}, Exec{cmd:String} }
                     // .weight(), .observes_content(), .label(), .range()
pub struct FileEvent { pub ts: Timestamp, pub agent: AgentId, pub session: Option<String>,
                       pub path: PathBuf, pub kind: EventKind, pub source: Source,
                       pub dedup_key: Option<String>, pub prompt_id: Option<String> }
impl FileEvent { fn new(ts, agent, path, kind, source) -> Self;
                 fn with_session(self, s) -> Self; fn with_dedup_key(self, k) -> Self;
                 fn with_prompt_id(self, id) -> Self }

// location.rs
pub const UNATTRIBUTED: &str = "_unattributed";
pub struct Workspace;   // ::new(root), .root(), .slug(), .relativize(&Path)
pub struct EventPaths;  // ::with_state_root(root, ws), ::discover(ws), .workspace(),
                        // .workspace_dir(), .session_dir(Option<&str>),
                        // .segment(Option<&str>, producer), .sessions()

// log.rs
pub const MAX_LINE_BYTES: usize = 4096;
pub enum LogError { Io{path,source}, Encode(serde_json::Error) }
pub struct Writer;             // ::open(&EventPaths, Option<&str>, producer), .path(), .append(&FileEvent)
pub struct ReadOutcome { pub events: Vec<FileEvent>, pub malformed_lines: usize, pub deduped: usize }
pub fn read_session(&EventPaths, Option<&str>) -> Result<ReadOutcome, LogError>;

// tracker.rs
pub enum Staleness { Unknown, Fresh, Modified }
pub struct KindCounts { pub read:u32, pub edit:u32, pub write:u32,
                        pub mention:u32, pub search:u32, pub exec:u32 }  // .total(), .mutated()
pub struct TrackedFile { pub path, pub first_seen, pub last_seen, pub counts,
                         pub last_range, pub last_content_seen, pub agents, pub pinned }
                         // .score_at(now, half_life), .staleness(), .staleness_with_mtime(Option<SystemTime>)
pub struct TrackerState; // ::new(Workspace, Option<String>), .apply(&FileEvent), .apply_all(iter),
                         // .ranked(now) -> Vec<&TrackedFile>, .get(&Path), .len(),
                         // .display_path(&TrackedFile), .set_pinned(&Path,bool), .latest_event()

// lib.rs
pub fn load_session(&EventPaths, Option<&str>) -> Result<(TrackerState, ReadOutcome), LogError>;
```

Verified hook payload shapes are in `docs/hook-payload.md`. Read it before Task 3.

## File Structure

| File | Responsibility |
|---|---|
| `crates/thirdeye-core/src/event.rs` | **modify** — add `EventKind::Flag`, `TurnMarker`, `TurnKind`, `LogRecord` |
| `crates/thirdeye-core/src/log.rs` | **modify** — read/write `LogRecord` instead of bare `FileEvent` |
| `crates/thirdeye-core/src/location.rs` | **modify** — runtime dir, socket path, endpoint file |
| `crates/thirdeye-adapters/src/claude.rs` | **create** — pure payload→records translation |
| `crates/thirdeye-adapters/src/lib.rs` | **create** — adapter errors, re-exports |
| `crates/thirdeye/src/main.rs` | **create** — CLI dispatch |
| `crates/thirdeye/src/cmd_hook.rs` | **create** — `thirdeye hook`: stdin → log |
| `crates/thirdeye/src/cmd_ls.rs` | **create** — `thirdeye ls`: log → turn-grouped output |
| `plugin/.claude-plugin/plugin.json` | **create** — plugin manifest |
| `plugin/hooks/hooks.json` | **create** — hook wiring |

---

### Task 1: Log record wrapper and turn markers

The log must carry turn boundaries, which have no file path. `FileEvent`'s invariant is that every event names a file, so turn markers become a sibling arm of a tagged `LogRecord` rather than pathless `EventKind` variants.

**Files:**
- Modify: `crates/thirdeye-core/src/event.rs`
- Modify: `crates/thirdeye-core/src/log.rs`
- Modify: `crates/thirdeye-core/src/lib.rs`

**Interfaces:**
- Consumes: existing `FileEvent`, `Timestamp`, `LineRange`.
- Produces: `LogRecord::{File,Turn}`, `TurnMarker { ts, session, prompt_id, kind }`, `TurnKind::{Open{prompt},Close}`, `EventKind::Flag { reason, range }`. `Writer::append(&LogRecord)`, `ReadOutcome::records: Vec<LogRecord>`.

- [ ] **Step 1: Write the failing round-trip test**

Add to the `tests` module at the bottom of `crates/thirdeye-core/src/event.rs`:

```rust
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
    let open = LogRecord::Turn(TurnMarker {
        ts: Timestamp(1_700_000_000_001),
        session: Some("s1".into()),
        prompt_id: "p1".into(),
        kind: TurnKind::Open { prompt: "fix dedup".into() },
    });
    let close = LogRecord::Turn(TurnMarker {
        ts: Timestamp(1_700_000_000_002),
        session: Some("s1".into()),
        prompt_id: "p1".into(),
        kind: TurnKind::Close,
    });

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
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p thirdeye-core log_records_round_trip -- --nocapture`
Expected: FAIL — `cannot find type LogRecord in this scope`.

- [ ] **Step 3: Add the types**

In `crates/thirdeye-core/src/event.rs`, add `Flag` to `EventKind` (place it after `Exec`):

```rust
    /// The agent explicitly asked the human to look at this file.
    Flag {
        reason: String,
        range: Option<LineRange>,
    },
```

Extend the three `EventKind` methods to cover it — a flag is the strongest
signal in the ranking, and it does not imply the agent read the content:

```rust
    // in weight(): add as the top arm
    EventKind::Flag { .. } => 8.0,
    // in label():  EventKind::Flag { .. } => "flag",
    // in range():  add Flag to the arm that returns *range
```

Then append the new types to the same file, before the `tests` module:

```rust
/// A conversational turn boundary. Has no path, so it is not a `FileEvent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMarker {
    pub ts: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Claude Code's `prompt_id`. Turn grouping depends entirely on this.
    pub prompt_id: String,
    #[serde(flatten)]
    pub kind: TurnKind,
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
}

impl LogRecord {
    pub fn ts(&self) -> Timestamp {
        match self {
            LogRecord::File(e) => e.ts,
            LogRecord::Turn(t) => t.ts,
        }
    }

    pub fn as_file(&self) -> Option<&FileEvent> {
        match self {
            LogRecord::File(e) => Some(e),
            LogRecord::Turn(_) => None,
        }
    }
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p thirdeye-core log_records_round_trip -- --nocapture`
Expected: PASS.

**If it fails on deserialization** rather than compilation, the cause is
serde's known interaction between `#[serde(flatten)]` and internally tagged
enums. Fix by switching `LogRecord` to adjacent tagging — change the attribute
to `#[serde(tag = "record", content = "data", rename_all = "snake_case")]` and
re-run. Do not remove the `flatten` on `FileEvent::kind`; the existing
`event_round_trips_through_json` test depends on the flat shape.

- [ ] **Step 5: Migrate the log to records**

In `crates/thirdeye-core/src/log.rs`:

Change the import to `use crate::event::{FileEvent, LogRecord};`

Change `Writer::append` to take a record, and truncation to reach through it:

```rust
    pub fn append(&mut self, record: &LogRecord) -> Result<(), LogError> {
        let mut line = serde_json::to_string(record)?;
        if line.len() + 1 > MAX_LINE_BYTES {
            let mut shrunk = record.clone();
            truncate_unbounded_fields(&mut shrunk, MAX_LINE_BYTES / 4);
            line = serde_json::to_string(&shrunk)?;
        }
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .map_err(io_err(&self.path))?;
        self.file.flush().map_err(io_err(&self.path))
    }
```

```rust
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
            _ => {}
        },
        LogRecord::Turn(marker) => {
            if let TurnKind::Open { prompt } = &mut marker.kind {
                clip(prompt)
            }
        }
    }
}
```

Rename the `ReadOutcome` field and keep a typed view of just the file events:

```rust
#[derive(Debug, Default)]
pub struct ReadOutcome {
    /// Merged, timestamp-ordered, deduplicated records.
    pub records: Vec<LogRecord>,
    pub malformed_lines: usize,
    pub deduped: usize,
}

impl ReadOutcome {
    /// File events only, in order. Turn markers are dropped.
    pub fn file_events(&self) -> impl Iterator<Item = &FileEvent> {
        self.records.iter().filter_map(LogRecord::as_file)
    }
}
```

In `read_session`, parse `LogRecord` instead of `FileEvent`, and sort on the
record timestamp with a path tie-break that turn markers sort stably under:

```rust
            match serde_json::from_str::<LogRecord>(&line) {
                Ok(record) => raw.push(record),
                Err(_) => outcome.malformed_lines += 1,
            }
```

```rust
    let before = raw.len();
    let mut records = dedup(raw);
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
```

Change `dedup` to operate on records — turn markers have no dedup key and pass
straight through:

```rust
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
                let candidate_is_push = record
                    .as_file()
                    .is_some_and(|e| e.source.is_push());
                if candidate_is_push && !incumbent_is_push {
                    out[idx] = None;
                    chosen.insert(key, out.len());
                    out.push(Some(record));
                }
            }
            None => {
                chosen.insert(key, out.len());
                out.push(Some(record));
            }
        }
    }

    out.into_iter().flatten().collect()
}
```

- [ ] **Step 6: Update existing call sites**

Every existing `log.rs` test calls `w.append(&ev(...))` and reads
`out.events`. Wrap the appends in `LogRecord::File(...)` and read
`out.records` / `out.file_events()`. In `crates/thirdeye-core/src/lib.rs`,
update `load_session` and the re-exports:

```rust
pub use event::{
    AgentId, EventKind, FileEvent, LineRange, LogRecord, Source, Timestamp, TurnKind, TurnMarker,
};
```

```rust
pub fn load_session(
    paths: &EventPaths,
    session: Option<&str>,
) -> Result<(TrackerState, ReadOutcome), LogError> {
    let outcome = read_session(paths, session)?;
    let mut state = TrackerState::new(paths.workspace().clone(), session.map(str::to_owned));
    state.apply_all(outcome.file_events());
    Ok((state, outcome))
}
```

- [ ] **Step 7: Run the full suite and gates**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: all 36+ tests PASS, clippy clean, fmt clean.

- [ ] **Step 8: Commit**

```bash
git add crates/thirdeye-core/src/
git commit -m "feat(core): add LogRecord wrapper, turn markers, and flag events"
```

---

### Task 2: Runtime paths and the socket-length guard

The measured constraint from the spec — a socket under the state dir is 111 bytes against a 108-byte limit — becomes a regression test so it cannot come back.

**Files:**
- Modify: `crates/thirdeye-core/src/location.rs`

**Interfaces:**
- Consumes: `EventPaths`, `Workspace`.
- Produces: `EventPaths::runtime_root() -> PathBuf`, `EventPaths::socket_path(&str) -> PathBuf`, `EventPaths::endpoint_file(Option<&str>) -> PathBuf`, `EventPaths::with_roots(state, runtime, ws)`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module at the bottom of `crates/thirdeye-core/src/location.rs`:

```rust
    /// Guards a real measured bug: a socket under the *state* dir is 111 bytes
    /// against sun_path's 108-byte limit on Linux (104 on macOS), so it cannot
    /// bind. Sockets must live in the short runtime dir instead.
    #[test]
    fn socket_paths_fit_inside_sun_path_on_every_platform() {
        const MACOS_SUN_PATH: usize = 104;
        let paths = EventPaths::with_roots(
            "/home/u/.local/state/thirdeye",
            "/run/user/1000/thirdeye",
            Workspace::new("/home/u/projects"),
        );

        let sock = paths.socket_path("4a818239-5b2a-45fc-95c0-a7de87e9e4be");
        let len = sock.to_string_lossy().len();
        assert!(
            len < MACOS_SUN_PATH,
            "socket path is {len} bytes, over the {MACOS_SUN_PATH}-byte macOS limit: {sock:?}"
        );

        // The descriptor may live in the long path; only the socket may not.
        assert!(
            paths
                .endpoint_file(Some("4a818239-5b2a-45fc-95c0-a7de87e9e4be"))
                .starts_with(paths.workspace_dir())
        );
    }

    #[test]
    fn socket_names_are_sanitized_like_session_dirs() {
        let paths = EventPaths::with_roots("/state", "/run/user/1000/thirdeye", Workspace::new("/w"));
        let sock = paths.socket_path("../../escape");
        assert_eq!(sock.parent().unwrap(), Path::new("/run/user/1000/thirdeye"));
        assert!(sock.extension().is_some_and(|e| e == "sock"));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p thirdeye-core socket_ -- --nocapture`
Expected: FAIL — `no function or associated item named with_roots`.

- [ ] **Step 3: Implement**

In `crates/thirdeye-core/src/location.rs`, add a `runtime_root` field to
`EventPaths` and the accessors. Replace the struct and add to the impl:

```rust
pub struct EventPaths {
    state_root: PathBuf,
    runtime_root: PathBuf,
    workspace: Workspace,
}
```

```rust
    /// Explicit roots. Tests and `--state-dir` use this.
    pub fn with_roots(
        state_root: impl Into<PathBuf>,
        runtime_root: impl Into<PathBuf>,
        workspace: Workspace,
    ) -> Self {
        Self {
            state_root: state_root.into(),
            runtime_root: runtime_root.into(),
            workspace,
        }
    }

    /// Where sockets live: `$XDG_RUNTIME_DIR/thirdeye`.
    ///
    /// Deliberately NOT under the state dir — see
    /// `socket_paths_fit_inside_sun_path_on_every_platform`.
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    pub fn socket_path(&self, session: &str) -> PathBuf {
        self.runtime_root
            .join(format!("{}.sock", sanitize_component(session)))
    }

    /// Transport descriptor. Lives in the long, browsable session dir; it is
    /// read as a file, so its length is unconstrained.
    pub fn endpoint_file(&self, session: Option<&str>) -> PathBuf {
        self.session_dir(session).join("endpoint.json")
    }
```

Keep `with_state_root` as a convenience that derives the runtime root, so the
existing tests keep compiling:

```rust
    pub fn with_state_root(state_root: impl Into<PathBuf>, workspace: Workspace) -> Self {
        Self::with_roots(state_root, Self::discover_runtime_root(), workspace)
    }

    fn discover_runtime_root() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join("thirdeye"))
            .unwrap_or_else(|| std::env::temp_dir().join("thirdeye"))
    }
```

Update `discover` to populate both roots:

```rust
    pub fn discover(workspace: Workspace) -> Self {
        let state_root = std::env::var_os("THIRDEYE_STATE_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_STATE_HOME").map(|d| PathBuf::from(d).join("thirdeye"))
            })
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state/thirdeye"))
            })
            .unwrap_or_else(|| PathBuf::from(".thirdeye"));
        Self::with_roots(state_root, Self::discover_runtime_root(), workspace)
    }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p thirdeye-core -- --nocapture`
Expected: PASS, including both new tests.

- [ ] **Step 5: Commit**

```bash
git add crates/thirdeye-core/src/location.rs
git commit -m "feat(core): add runtime dir paths with a sun_path length guard"
```

---

### Task 3: Claude Code payload translation

A pure function from a hook payload to log records. No I/O, so every case is testable against the shapes captured in `docs/hook-payload.md`.

**Files:**
- Create: `crates/thirdeye-adapters/Cargo.toml`
- Create: `crates/thirdeye-adapters/src/lib.rs`
- Create: `crates/thirdeye-adapters/src/claude.rs`
- Modify: `Cargo.toml` (workspace members)

**Interfaces:**
- Consumes: `LogRecord`, `FileEvent`, `EventKind`, `TurnMarker`, `TurnKind`, `Timestamp`, `AgentId`, `Source`, `LineRange`.
- Produces: `thirdeye_adapters::claude::records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError>` where `Payload { pub session: Option<String>, pub cwd: Option<PathBuf>, pub records: Vec<LogRecord> }`; `AdapterError`.

- [ ] **Step 1: Create the crate**

`crates/thirdeye-adapters/Cargo.toml`:

```toml
[package]
name = "thirdeye-adapters"
description = "Translates agent-specific transports into thirdeye log records"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
thirdeye-core.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true

[lints]
workspace = true
```

In the root `Cargo.toml`, set members and add the dependency:

```toml
members = ["crates/thirdeye-core", "crates/thirdeye-adapters"]
```

```toml
thirdeye-adapters = { path = "crates/thirdeye-adapters" }
```

- [ ] **Step 2: Write the failing tests**

`crates/thirdeye-adapters/src/claude.rs` — write the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const NOW: Timestamp = Timestamp(1_700_000_000_000);

    /// Verbatim shape from docs/hook-payload.md.
    const READ_PAYLOAD: &str = r#"{
      "session_id": "328abcfc-2bd5-4aa9-8aa8-07d14569d550",
      "transcript_path": "/home/u/.claude/projects/x/y.jsonl",
      "cwd": "/home/u/app",
      "prompt_id": "838748f2-65af-4e75-948b-5629215d63b9",
      "permission_mode": "default",
      "hook_event_name": "PostToolUse",
      "tool_name": "Read",
      "tool_input": { "file_path": "/home/u/app/src/log.rs" },
      "tool_response": { "type": "text", "file": {
          "filePath": "/home/u/app/src/log.rs",
          "content": "…", "numLines": 8, "startLine": 1, "totalLines": 8 } },
      "tool_use_id": "toolu_011NdbU3kC4B29Gh2uqYTvJJ",
      "duration_ms": 1
    }"#;

    #[test]
    fn read_payload_becomes_a_ranged_read_event() {
        let out = records_from_payload(READ_PAYLOAD, NOW).unwrap();

        assert_eq!(out.session.as_deref(), Some("328abcfc-2bd5-4aa9-8aa8-07d14569d550"));
        assert_eq!(out.cwd.as_deref(), Some(Path::new("/home/u/app")));
        assert_eq!(out.records.len(), 1);

        let ev = out.records[0].as_file().unwrap();
        assert_eq!(ev.path, Path::new("/home/u/app/src/log.rs"));
        assert_eq!(ev.source, Source::Hook);
        assert_eq!(ev.agent, AgentId::Claude);
        assert_eq!(ev.dedup_key.as_deref(), Some("toolu_011NdbU3kC4B29Gh2uqYTvJJ"));
        assert_eq!(ev.prompt_id.as_deref(), Some("838748f2-65af-4e75-948b-5629215d63b9"));
        assert_eq!(
            ev.kind,
            EventKind::Read { range: Some(LineRange::new(1, 9)) },
            "range must come from tool_response startLine/numLines"
        );
    }

    #[test]
    fn tool_response_content_is_never_retained() {
        // The payload embeds the whole file; the log must not become a second
        // copy of the user's source tree.
        let out = records_from_payload(READ_PAYLOAD, NOW).unwrap();
        let line = serde_json::to_string(&out.records[0]).unwrap();
        assert!(!line.contains("content"), "leaked tool_response content: {line}");
        assert!(line.len() < 4096, "line must stay under MAX_LINE_BYTES");
    }

    fn tool_payload(tool: &str, input: &str) -> String {
        format!(
            r#"{{"session_id":"s1","cwd":"/w","prompt_id":"p1",
                 "hook_event_name":"PostToolUse","tool_name":"{tool}",
                 "tool_input":{input},"tool_use_id":"t1"}}"#
        )
    }

    #[test]
    fn mutating_tools_map_to_mutating_kinds() {
        for (tool, input) in [
            ("Edit", r#"{"file_path":"/w/a.rs"}"#),
            ("Write", r#"{"file_path":"/w/a.rs"}"#),
            ("NotebookEdit", r#"{"notebook_path":"/w/a.ipynb"}"#),
        ] {
            let out = records_from_payload(&tool_payload(tool, input), NOW).unwrap();
            let ev = out.records[0].as_file().unwrap();
            assert!(
                matches!(ev.kind, EventKind::Edit { .. } | EventKind::Write),
                "{tool} produced {:?}",
                ev.kind
            );
        }
    }

    #[test]
    fn search_tools_record_their_pattern_and_path() {
        let out = records_from_payload(
            &tool_payload("Grep", r#"{"pattern":"fn dedup","path":"/w/src"}"#),
            NOW,
        )
        .unwrap();
        let ev = out.records[0].as_file().unwrap();
        assert_eq!(ev.path, Path::new("/w/src"));
        assert_eq!(ev.kind, EventKind::Search { pattern: "fn dedup".into() });
    }

    #[test]
    fn prompt_submission_opens_a_turn_and_extracts_mentions() {
        let raw = r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1",
          "hook_event_name":"UserPromptSubmit",
          "prompt":"please review @src/log.rs and @docs/notes.md"}"#;
        let out = records_from_payload(raw, NOW).unwrap();

        let turn = match &out.records[0] {
            LogRecord::Turn(t) => t,
            other => panic!("expected a turn marker, got {other:?}"),
        };
        assert_eq!(turn.prompt_id, "p1");
        assert!(matches!(turn.kind, TurnKind::Open { .. }));

        let mentions: Vec<_> = out
            .records
            .iter()
            .filter_map(LogRecord::as_file)
            .filter(|e| e.kind == EventKind::Mention)
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(
            mentions,
            vec![Path::new("/w/src/log.rs"), Path::new("/w/docs/notes.md")],
            "mentions must be resolved against cwd"
        );
    }

    #[test]
    fn stop_closes_the_turn() {
        let raw = r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1","hook_event_name":"Stop"}"#;
        let out = records_from_payload(raw, NOW).unwrap();
        assert_eq!(out.records.len(), 1);
        assert!(matches!(
            &out.records[0],
            LogRecord::Turn(TurnMarker { kind: TurnKind::Close, .. })
        ));
    }

    #[test]
    fn unhandled_events_and_pathless_tools_produce_nothing() {
        // Bash path extraction is fuzzy and deliberately out of PoC scope;
        // producing nothing is correct, erroring is not.
        for raw in [
            &tool_payload("Bash", r#"{"command":"ls -la /w"}"#),
            &tool_payload("WebFetch", r#"{"url":"https://example.com"}"#),
            r#"{"session_id":"s1","hook_event_name":"SessionStart","cwd":"/w"}"#.to_string().as_str(),
        ] {
            let out = records_from_payload(raw, NOW).unwrap();
            assert!(out.records.is_empty(), "expected no records from {raw}");
        }
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(records_from_payload("{not json", NOW).is_err());
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p thirdeye-adapters -- --nocapture`
Expected: FAIL — the module has no implementation yet.

- [ ] **Step 4: Implement the translation**

`crates/thirdeye-adapters/src/lib.rs`:

```rust
//! Adapters translate agent-specific transports into `LogRecord`s.
//!
//! Heads never see these formats — that separation is what lets a new agent
//! and a new head be added independently.

pub mod claude;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("could not parse hook payload: {0}")]
    Parse(#[from] serde_json::Error),
}
```

Prepend to `crates/thirdeye-adapters/src/claude.rs` (above the test module):

```rust
//! Claude Code hook payload → log records.
//!
//! Payload shapes are verified, not assumed — see `docs/hook-payload.md`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use thirdeye_core::{
    AgentId, EventKind, FileEvent, LineRange, LogRecord, Source, Timestamp, TurnKind, TurnMarker,
};

use crate::AdapterError;

/// What one hook invocation yielded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Payload {
    pub session: Option<String>,
    pub cwd: Option<PathBuf>,
    pub records: Vec<LogRecord>,
}

/// Translate one hook payload. Unrecognized events yield no records rather
/// than an error: Claude Code may add hook events and tools at any time, and a
/// hook that fails on an unknown shape would break the user's agent.
pub fn records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    let v: Value = serde_json::from_str(raw)?;

    let session = v["session_id"].as_str().map(str::to_owned);
    let cwd = v["cwd"].as_str().map(PathBuf::from);
    let prompt_id = v["prompt_id"].as_str().map(str::to_owned);
    let mut out = Payload {
        session: session.clone(),
        cwd: cwd.clone(),
        records: Vec::new(),
    };

    let event = v["hook_event_name"].as_str().unwrap_or_default();
    match event {
        "PostToolUse" => {
            if let Some(kind_and_path) = tool_to_event(&v) {
                let (path, kind) = kind_and_path;
                let mut ev = FileEvent::new(
                    now,
                    AgentId::Claude,
                    absolutize(&path, cwd.as_deref()),
                    kind,
                    Source::Hook,
                );
                ev.session = session;
                ev.prompt_id = prompt_id;
                ev.dedup_key = v["tool_use_id"].as_str().map(str::to_owned);
                out.records.push(LogRecord::File(ev));
            }
        }
        "UserPromptSubmit" => {
            let Some(prompt_id) = prompt_id else {
                return Ok(out);
            };
            let prompt = v["prompt"].as_str().unwrap_or_default().to_string();
            out.records.push(LogRecord::Turn(TurnMarker {
                ts: now,
                session: session.clone(),
                prompt_id: prompt_id.clone(),
                kind: TurnKind::Open {
                    prompt: prompt.clone(),
                },
            }));
            for mention in extract_mentions(&prompt) {
                let mut ev = FileEvent::new(
                    now,
                    AgentId::Claude,
                    absolutize(Path::new(&mention), cwd.as_deref()),
                    EventKind::Mention,
                    Source::Hook,
                );
                ev.session = session.clone();
                ev.prompt_id = Some(prompt_id.clone());
                out.records.push(LogRecord::File(ev));
            }
        }
        "Stop" => {
            if let Some(prompt_id) = prompt_id {
                out.records.push(LogRecord::Turn(TurnMarker {
                    ts: now,
                    session,
                    prompt_id,
                    kind: TurnKind::Close,
                }));
            }
        }
        _ => {}
    }

    Ok(out)
}

/// Map a tool call to (path, kind). `None` for tools with no single file
/// subject — `Bash` path extraction is fuzzy and out of PoC scope.
fn tool_to_event(v: &Value) -> Option<(PathBuf, EventKind)> {
    let tool = v["tool_name"].as_str()?;
    let input = &v["tool_input"];
    let file_path = |key: &str| input[key].as_str().map(PathBuf::from);

    match tool {
        "Read" => Some((file_path("file_path")?, EventKind::Read { range: read_range(v) })),
        "Edit" | "MultiEdit" => {
            Some((file_path("file_path")?, EventKind::Edit { range: read_range(v) }))
        }
        "NotebookEdit" => Some((file_path("notebook_path")?, EventKind::Edit { range: None })),
        "Write" => Some((file_path("file_path")?, EventKind::Write)),
        "Grep" | "Glob" => {
            let pattern = input["pattern"].as_str().unwrap_or_default().to_string();
            let path = file_path("path").unwrap_or_else(|| PathBuf::from("."));
            Some((path, EventKind::Search { pattern }))
        }
        _ => None,
    }
}

/// The touched range, straight from `tool_response.file`. Correct even when
/// the agent read a whole file, unlike reconstructing from offset/limit.
fn read_range(v: &Value) -> Option<LineRange> {
    let file = &v["tool_response"]["file"];
    let start = file["startLine"].as_u64()? as u32;
    let lines = file["numLines"].as_u64()? as u32;
    Some(LineRange::new(start, start + lines))
}

fn absolutize(path: &Path, cwd: Option<&Path>) -> PathBuf {
    match (path.is_absolute(), cwd) {
        (true, _) | (false, None) => path.to_path_buf(),
        (false, Some(cwd)) => cwd.join(path),
    }
}

/// `@path` references in a prompt. Trailing sentence punctuation is stripped
/// so "review @src/log.rs." does not yield a path ending in a dot.
fn extract_mentions(prompt: &str) -> Vec<String> {
    prompt
        .split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
        .map(|w| w.trim_end_matches(['.', ',', ';', ':', ')', ']']).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p thirdeye-adapters -- --nocapture`
Expected: all 8 tests PASS.

- [ ] **Step 6: Run the gates and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
git add Cargo.toml crates/thirdeye-adapters/
git commit -m "feat(adapters): translate Claude Code hook payloads into log records"
```

---

### Task 4: The `thirdeye hook` binary

**Files:**
- Create: `crates/thirdeye/Cargo.toml`
- Create: `crates/thirdeye/src/main.rs`
- Create: `crates/thirdeye/src/cmd_hook.rs`
- Modify: `Cargo.toml` (workspace members, clap dependency)

**Interfaces:**
- Consumes: `records_from_payload`, `Payload`, `EventPaths::discover`, `Workspace::new`, `Writer`.
- Produces: binary `thirdeye` with subcommand `hook`; `cmd_hook::run(reader: impl Read) -> anyhow::Result<usize>` returning the number of records written.

- [ ] **Step 1: Create the crate**

`crates/thirdeye/Cargo.toml`:

```toml
[package]
name = "thirdeye"
description = "Companion viewer for CLI coding agents"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
thirdeye-core.workspace = true
thirdeye-adapters.workspace = true
anyhow.workspace = true
clap.workspace = true

[lints]
workspace = true
```

Root `Cargo.toml`:

```toml
members = ["crates/thirdeye-core", "crates/thirdeye-adapters", "crates/thirdeye"]
```

```toml
anyhow = "1"
clap = { version = "4", features = ["derive"] }
```

- [ ] **Step 2: Write the failing test**

`crates/thirdeye/src/cmd_hook.rs` — tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{EventPaths, Workspace, load_session};

    fn temp_roots(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("thirdeye-hook-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        (base.join("state"), base.join("run"))
    }

    #[test]
    fn a_read_payload_lands_in_the_session_log() {
        let (state, run) = temp_roots("read");
        let raw = r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1",
          "hook_event_name":"PostToolUse","tool_name":"Write",
          "tool_input":{"file_path":"/w/src/a.rs"},"tool_use_id":"t1"}"#;

        let written = run_with_roots(raw.as_bytes(), &state, &run).unwrap();
        assert_eq!(written, 1);

        let paths = EventPaths::with_roots(&state, &run, Workspace::new("/w"));
        let (tracker, outcome) = load_session(&paths, Some("s1")).unwrap();
        assert_eq!(outcome.malformed_lines, 0);
        assert_eq!(tracker.len(), 1);
        assert!(tracker.get(std::path::Path::new("/w/src/a.rs")).is_some());
    }

    #[test]
    fn an_unhandled_event_writes_nothing_and_still_succeeds() {
        // A hook that errors would surface as noise in the user's agent.
        let (state, run) = temp_roots("noop");
        let raw = r#"{"session_id":"s1","cwd":"/w","hook_event_name":"Notification"}"#;
        assert_eq!(run_with_roots(raw.as_bytes(), &state, &run).unwrap(), 0);
    }

    #[test]
    fn malformed_input_reports_an_error_without_panicking() {
        let (state, run) = temp_roots("bad");
        assert!(run_with_roots(b"{not json".as_slice(), &state, &run).is_err());
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p thirdeye -- --nocapture`
Expected: FAIL — `cannot find function run_with_roots`.

- [ ] **Step 4: Implement**

Prepend to `crates/thirdeye/src/cmd_hook.rs`:

```rust
//! `thirdeye hook` — the binary a Claude Code hook shells out to.
//!
//! Runs on every mutation, so it does the minimum: parse stdin, append,
//! exit. It never touches the terminal; that is the daemon's job.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use thirdeye_adapters::claude;
use thirdeye_core::{EventPaths, Timestamp, Workspace, Writer};

/// Producer name for this adapter's log segment.
const PRODUCER: &str = "hook";

pub fn run(reader: impl Read) -> Result<usize> {
    let paths = |ws: Workspace| EventPaths::discover(ws);
    run_inner(reader, paths)
}

#[cfg(test)]
fn run_with_roots(reader: impl Read, state: &Path, runtime: &Path) -> Result<usize> {
    let (state, runtime) = (state.to_path_buf(), runtime.to_path_buf());
    run_inner(reader, move |ws| {
        EventPaths::with_roots(state.clone(), runtime.clone(), ws)
    })
}

fn run_inner(mut reader: impl Read, make_paths: impl Fn(Workspace) -> EventPaths) -> Result<usize> {
    let mut raw = String::new();
    reader
        .read_to_string(&mut raw)
        .context("reading hook payload from stdin")?;

    let payload = claude::records_from_payload(&raw, Timestamp::now())?;
    if payload.records.is_empty() {
        return Ok(0);
    }

    let workspace = Workspace::new(workspace_root(payload.cwd.as_deref()));
    let paths = make_paths(workspace);
    let mut writer = Writer::open(&paths, payload.session.as_deref(), PRODUCER)?;
    for record in &payload.records {
        writer.append(record)?;
    }
    Ok(payload.records.len())
}

/// `$CLAUDE_PROJECT_DIR` is the project root; `cwd` is where the agent
/// happens to be. Prefer the former so a subdirectory does not fork a
/// second workspace.
fn workspace_root(cwd: Option<&Path>) -> PathBuf {
    std::env::var_os("CLAUDE_PROJECT_DIR")
        .map(PathBuf::from)
        .or_else(|| cwd.map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}
```

`crates/thirdeye/src/main.rs`:

```rust
mod cmd_hook;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "thirdeye", version, about = "Companion viewer for CLI coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Consume an agent hook payload on stdin and append it to the log.
    Hook,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Hook => {
            cmd_hook::run(std::io::stdin().lock())?;
            Ok(())
        }
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p thirdeye -- --nocapture`
Expected: 3 tests PASS.

- [ ] **Step 6: Verify by hand**

```bash
export THIRDEYE_STATE_DIR=/tmp/thirdeye-manual
echo '{"session_id":"s1","cwd":"/w","prompt_id":"p1","hook_event_name":"PostToolUse","tool_name":"Write","tool_input":{"file_path":"/w/a.rs"},"tool_use_id":"t1"}' \
  | cargo run -q -p thirdeye -- hook
find /tmp/thirdeye-manual -name '*.jsonl' -exec cat {} \;
```

Expected: one JSON line with `"record":"file"`, `"kind":"write"`, and **no** file content.

- [ ] **Step 7: Commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
git add Cargo.toml crates/thirdeye/
git commit -m "feat(cli): add thirdeye hook to append agent events to the log"
```

---

### Task 5: `thirdeye ls` — turn-grouped output

The demonstrable deliverable. Turn grouping is the piece the daemon and TUI will both build on, so it gets built and tested here, headless.

**Files:**
- Create: `crates/thirdeye-core/src/turns.rs`
- Modify: `crates/thirdeye-core/src/lib.rs`
- Create: `crates/thirdeye/src/cmd_ls.rs`
- Modify: `crates/thirdeye/src/main.rs`

**Interfaces:**
- Consumes: `ReadOutcome`, `LogRecord`, `FileEvent`, `TurnKind`.
- Produces: `thirdeye_core::turns::{Turn, group_into_turns}` where `Turn { pub prompt_id: Option<String>, pub prompt: Option<String>, pub opened_at: Timestamp, pub closed: bool, pub events: Vec<FileEvent> }` and `group_into_turns(&[LogRecord]) -> Vec<Turn>`.

- [ ] **Step 1: Write the failing tests**

`crates/thirdeye-core/src/turns.rs` — tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentId, EventKind, FileEvent, Source, TurnMarker};

    fn open(ts: u64, id: &str) -> LogRecord {
        LogRecord::Turn(TurnMarker {
            ts: Timestamp(ts),
            session: Some("s1".into()),
            prompt_id: id.into(),
            kind: TurnKind::Open { prompt: format!("prompt {id}") },
        })
    }

    fn close(ts: u64, id: &str) -> LogRecord {
        LogRecord::Turn(TurnMarker {
            ts: Timestamp(ts),
            session: Some("s1".into()),
            prompt_id: id.into(),
            kind: TurnKind::Close,
        })
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
    fn events_without_a_prompt_id_land_in_a_leading_ungrouped_turn() {
        // An adapter that cannot supply a turn id degrades, never drops data.
        let turns = group_into_turns(&[edit(1, "/a.rs", None), open(2, "p1"), edit(3, "/b.rs", Some("p1"))]);

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
    fn empty_input_yields_no_turns() {
        assert!(group_into_turns(&[]).is_empty());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p thirdeye-core turns:: -- --nocapture`
Expected: FAIL — module `turns` does not exist.

- [ ] **Step 3: Implement**

Prepend to `crates/thirdeye-core/src/turns.rs`:

```rust
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
        let existing = turns
            .iter()
            .position(|t| t.prompt_id.as_deref() == id);
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
                        turn.prompt = Some(prompt.clone());
                        turn.opened_at = marker.ts;
                    }
                    TurnKind::Close => turn.closed = true,
                }
            }
            LogRecord::File(event) => {
                let turn = slot(&mut turns, event.prompt_id.as_deref(), event.ts);
                turn.events.push(event.clone());
            }
        }
    }

    turns
}
```

Register the module in `crates/thirdeye-core/src/lib.rs`:

```rust
pub mod turns;
```

```rust
pub use turns::{Turn, group_into_turns};
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p thirdeye-core turns:: -- --nocapture`
Expected: 5 tests PASS.

- [ ] **Step 5: Write the `ls` command**

`crates/thirdeye/src/cmd_ls.rs`:

```rust
//! `thirdeye ls` — print the turn-grouped file set for a session.
//!
//! Headless counterpart to the TUI, and the way the capture pipeline is
//! verified before any viewer exists.

use anyhow::{Result, bail};
use thirdeye_core::{EventPaths, Workspace, group_into_turns, read_session};

pub fn run(session: Option<String>, workspace: Option<std::path::PathBuf>) -> Result<()> {
    let root = match workspace {
        Some(w) => w,
        None => std::env::current_dir()?,
    };
    let paths = EventPaths::discover(Workspace::new(root));

    let session = match session {
        Some(s) => s,
        None => {
            let mut found = paths.sessions()?;
            match found.len() {
                0 => bail!("no sessions recorded for {}", paths.workspace_dir().display()),
                1 => found.remove(0),
                _ => bail!("{} sessions recorded; pass --session <id>", found.len()),
            }
        }
    };

    let outcome = read_session(&paths, Some(&session))?;
    if outcome.malformed_lines > 0 {
        eprintln!("warning: {} malformed log lines skipped", outcome.malformed_lines);
    }

    let turns = group_into_turns(&outcome.records);
    if turns.is_empty() {
        println!("no events recorded for session {session}");
        return Ok(());
    }

    for (i, turn) in turns.iter().enumerate() {
        let marker = if turn.closed { " " } else { "*" };
        let label = turn.prompt.as_deref().unwrap_or("(no prompt recorded)");
        println!("{marker} turn {} — {}", i + 1, first_line(label, 60));

        let mut paths_seen: Vec<_> = turn.events.iter().map(|e| &e.path).collect();
        paths_seen.sort();
        paths_seen.dedup();

        for path in paths_seen {
            let kinds: Vec<&str> = {
                let mut k: Vec<&str> = turn
                    .events
                    .iter()
                    .filter(|e| &e.path == path)
                    .map(|e| e.kind.label())
                    .collect();
                k.sort_unstable();
                k.dedup();
                k
            };
            println!(
                "    {:<50} {}",
                paths.workspace().relativize(path).display(),
                kinds.join(",")
            );
        }
    }
    Ok(())
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or_default();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_string()
    }
}
```

Wire it into `crates/thirdeye/src/main.rs`:

```rust
mod cmd_hook;
mod cmd_ls;
```

```rust
#[derive(Subcommand)]
enum Command {
    /// Consume an agent hook payload on stdin and append it to the log.
    Hook,
    /// Print the turn-grouped file set for a session.
    Ls {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        workspace: Option<std::path::PathBuf>,
    },
}
```

```rust
fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Hook => {
            cmd_hook::run(std::io::stdin().lock())?;
            Ok(())
        }
        Command::Ls { session, workspace } => cmd_ls::run(session, workspace),
    }
}
```

- [ ] **Step 6: Run gates and commit**

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
git add crates/
git commit -m "feat: group log records into turns and add thirdeye ls"
```

---

### Task 6: Plugin packaging and end-to-end verification

**Files:**
- Create: `plugin/.claude-plugin/plugin.json`
- Create: `plugin/hooks/hooks.json`
- Create: `plugin/scripts/thirdeye-hook.sh`
- Create: `docs/install.md`
- Delete: `.claude/spike/` (M0 scaffolding, superseded by `docs/hook-payload.md`)

**Interfaces:**
- Consumes: the `thirdeye` binary from Task 4.
- Produces: an installable plugin directory.

- [ ] **Step 1: Write the plugin manifest**

`plugin/.claude-plugin/plugin.json`:

```json
{
  "name": "thirdeye",
  "description": "Tracks the files your agent touches and shows them in a live viewer",
  "version": "0.1.0",
  "license": "MIT",
  "keywords": ["viewer", "files", "review"]
}
```

- [ ] **Step 2: Write the hook wiring**

`plugin/hooks/hooks.json`. `UserPromptSubmit` and `Stop` take no matcher;
`PostToolUse` filters to the tools the adapter understands:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Read|Edit|MultiEdit|Write|NotebookEdit|Grep|Glob",
        "hooks": [
          {
            "type": "command",
            "command": "\"${CLAUDE_PLUGIN_ROOT}/scripts/thirdeye-hook.sh\"",
            "shell": "bash",
            "async": true
          }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "\"${CLAUDE_PLUGIN_ROOT}/scripts/thirdeye-hook.sh\"",
            "shell": "bash",
            "async": true
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "\"${CLAUDE_PLUGIN_ROOT}/scripts/thirdeye-hook.sh\"",
            "shell": "bash",
            "async": true
          }
        ]
      }
    ]
  }
}
```

- [ ] **Step 3: Write the wrapper script**

`plugin/scripts/thirdeye-hook.sh`. It must never fail the user's agent, so it
swallows every error:

```bash
#!/usr/bin/env bash
# Feed a Claude Code hook payload to thirdeye.
#
# Exits 0 unconditionally: a broken viewer must never break the user's agent.
# Diagnostics go to $XDG_STATE_HOME/thirdeye/hook-errors.log, not to stderr,
# because stderr from a hook surfaces inside the agent session.

set -u
bin="$(command -v thirdeye || echo "$HOME/.local/bin/thirdeye")"
[ -x "$bin" ] || exit 0

errlog="${XDG_STATE_HOME:-$HOME/.local/state}/thirdeye/hook-errors.log"
mkdir -p "$(dirname "$errlog")" 2>/dev/null || exit 0

"$bin" hook 2>>"$errlog" || true
exit 0
```

Make it executable:

```bash
chmod +x plugin/scripts/thirdeye-hook.sh
```

- [ ] **Step 4: Install the binary and plugin**

> **Correction (verified after execution).** The commands below are **wrong**
> and were never run. `~/.claude/plugins/local/` does not exist — Claude Code
> installs plugins from *marketplaces*, tracked in
> `~/.claude/plugins/known_marketplaces.json`. Symlinking into a `local/`
> directory creates something nothing reads. The real flow, verified against
> Claude Code 2.1.220 and now in `docs/install.md`, makes the repo root a
> marketplace via `.claude-plugin/marketplace.json` and installs with
> `claude plugin marketplace add <path> --scope project` +
> `claude plugin install thirdeye@thirdeye --scope project`. Left here
> unedited as a record of what the plan actually said.

```bash
cargo install --path crates/thirdeye --root ~/.local
mkdir -p ~/.claude/plugins/local
ln -sfn "$PWD/plugin" ~/.claude/plugins/local/thirdeye
```

Then add to `~/.claude/settings.json` under `enabledPlugins`, or install via
`/plugin`. Record whichever worked in `docs/install.md`.

- [ ] **Step 5: End-to-end verification in a real session**

Project `.claude/settings.json` is read at session start, so this needs a
fresh process:

```bash
cd /tmp && rm -rf thirdeye-e2e && mkdir thirdeye-e2e && cd thirdeye-e2e
git init -q && printf 'fn main() {}\n' > a.rs && printf 'fn helper() {}\n' > b.rs
claude -p "Read a.rs, then add a doc comment to b.rs" --allowedTools "Read,Edit"
thirdeye ls
```

Expected output — two turns are not required, but one turn containing both
files with the right kinds is:

```
* turn 1 — Read a.rs, then add a doc comment to b.rs
    a.rs                                               read
    b.rs                                               edit,read
```

- [ ] **Step 6: Verify the two properties that matter**

```bash
# 1. No file content leaked into the log.
find ~/.local/state/thirdeye -name '*.jsonl' -exec grep -l '"content"' {} \; \
  | grep . && echo "FAIL: content leaked" || echo "OK: no content in log"

# 2. Every line is under MAX_LINE_BYTES.
find ~/.local/state/thirdeye -name '*.jsonl' -exec awk \
  'length($0) >= 4096 { print FILENAME": "length($0); f=1 } END { exit f }' {} +\
  && echo "OK: all lines under 4096"
```

Expected: both OK.

- [ ] **Step 7: Remove the M0 spike**

Its findings are recorded in `docs/hook-payload.md`, and the real hook wiring
supersedes it:

```bash
git rm -r --cached .claude/spike 2>/dev/null || true
rm -rf .claude/spike
```

Remove the now-dangling `/.claude/spike/payloads.jsonl` line from `.gitignore`,
and remove the spike hook block from `.claude/settings.json`.

- [ ] **Step 8: Document and commit**

Write `docs/install.md` covering: build, `cargo install`, plugin linking,
enabling, and the "restart your agent — settings load at session start" note.

```bash
cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
git add plugin/ docs/install.md .gitignore .claude/
git commit -m "feat: package thirdeye as a Claude Code plugin"
```

---

## Self-review

**Spec coverage.** This plan covers spec §16 milestones 1 and 2 in full, plus
the turn model from §9 and the log/runtime layout from §7. Deliberately
deferred to later plans, each named in its spec section:

| Spec section | Milestone | Plan |
|---|---|---|
| §6 activation, §4.1 daemon | 3 | plan 2 |
| §11 head protocol | 4 | plan 2 |
| §10.1 TUI layout | 5 | plan 2 |
| §12 kitty surface, §5 `Surface` | 6 | plan 2 |
| §10.3 policy, slash commands | 7 | plan 3 |
| §9 flag tool, MCP server | 8 | plan 3 |
| §8 rootless unit installer | 9 | plan 3 |
| §14 kitty smoke test | 10 | plan 3 |

`EventKind::Flag` is added here rather than in plan 3 because changing the log
format once, early, is cheaper than migrating it later — the MCP server that
*emits* flags lands in plan 3.

**Placeholder scan.** No TBD/TODO. Every code step carries real code. The one
conditional instruction (Task 1 Step 4's serde fallback) names the exact
attribute to change and the exact symptom that triggers it.

**Type consistency.** `records_from_payload` returns `Payload` in Task 3 and is
consumed as `payload.records` / `payload.session` / `payload.cwd` in Task 4.
`ReadOutcome.records` (renamed from `.events` in Task 1) is consumed by
`group_into_turns` in Task 5 and `.file_events()` by `load_session`.
`EventPaths::with_roots` is defined in Task 2 and used in Tasks 4 and 5.
`LogRecord::as_file` is defined in Task 1 and used in Tasks 3, 4 and 5.
`EventKind::label()` already exists in core and is used by `cmd_ls`.

**Known risk.** Task 1 Step 4 depends on serde's `flatten` behaving inside an
internally-tagged enum. The round-trip test is written before the type exists
precisely so this surfaces immediately, with the fallback documented in place.
