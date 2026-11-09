//! What a tool call wrote, found by watching the workspace across it.
//!
//! For tools whose payload cannot say what they changed — anything that runs
//! an arbitrary command. Codex makes every edit that way (a captured one was
//! a Python heredoc), and Claude Code's `Bash` does too; interpreting the
//! command text would put files in the list that were never touched.
//!
//! The bracket is two hook calls. `PreToolUse` leaves a marker file whose
//! mtime is the start of the window; `PostToolUse` reports every workspace
//! file modified between that mtime and now. The marker's own timestamp is
//! used rather than a recorded clock reading so both ends come from the same
//! clock the files are stamped with.
//!
//! What it cannot tell apart, by construction: a file the human saved while
//! the command ran. It is reported as the agent's write.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

/// Most files a single observation will `stat`.
///
/// Run from a home directory instead of a project, an uncapped walk would
/// visit everything the user owns on every tool call.
pub const WALK_CAP: usize = 50_000;

/// Directories the walk outside a repository never enters, besides hidden
/// ones: dependency and build trees, rewritten wholesale by `npm install` or
/// a build and never what the human is looking for. Inside a repository,
/// `.gitignore` decides instead.
const SKIPPED_DIRS: &[&str] = &["node_modules", "target", "__pycache__"];

/// Where a session's markers live: beside its socket and session pointer, in
/// the runtime directory, where per-session ephemera belong.
pub fn markers_for(socket: &Path) -> PathBuf {
    socket.with_extension("calls")
}

/// What a call wrote, and what it deleted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub written: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
}

/// What existed when a call began, kept in its marker: deletion is the
/// difference at the end. Two sources, because nothing else is cheap enough
/// to snapshot before every shell command — the files this session already
/// knows, and git's tracked files already missing. A file in neither (never
/// touched, untracked or outside a repository) is gone unseen.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Snapshot {
    known: Vec<PathBuf>,
    git_missing: Vec<PathBuf>,
}

/// Open the window for `call`. `known` is every file the session has
/// recorded so far.
pub fn begin(markers: &Path, call: &str, root: &Path, known: &[PathBuf]) -> std::io::Result<()> {
    std::fs::create_dir_all(markers)?;
    // Once each, and only this workspace's: a session can record /tmp
    // paths, and another program removing one is not this agent's deletion.
    let mut known: Vec<PathBuf> = known
        .iter()
        .filter(|p| p.starts_with(root) && p.exists())
        .cloned()
        .collect();
    known.sort();
    known.dedup();
    let snapshot = Snapshot {
        known,
        git_missing: git_missing(root),
    };
    // Writing truncates and rewrites the mtime, so a retried call id restarts
    // its own window rather than inheriting a stale one.
    std::fs::write(
        markers.join(marker_name(call)),
        serde_json::to_vec(&snapshot).unwrap_or_default(),
    )
}

/// Tracked files missing from the work tree, absolute; empty outside a
/// repository.
fn git_missing(root: &Path) -> Vec<PathBuf> {
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--deleted"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    out.stdout
        .split(|b| *b == 0)
        .filter(|name| !name.is_empty())
        .map(|name| root.join(String::from_utf8_lossy(name).as_ref()))
        .collect()
}

/// Close the window for `call`: the absolute paths of the files under `root`
/// modified inside it, and of the files that existed at its start and do
/// not now.
///
/// `now` is taken by the caller *before* this walks: with async hooks the
/// next command may already be running, and its writes are its own.
pub fn end(markers: &Path, call: &str, root: &Path, now: SystemTime) -> Observed {
    let name = marker_name(call);
    let marker = markers.join(&name);
    let Ok(start) = std::fs::metadata(&marker).and_then(|m| m.modified()) else {
        // No PreToolUse for this call — hooks installed mid-command, or one
        // that never ran. There is no window, and guessing one misattributes.
        return Observed::default();
    };
    // A marker from before snapshots existed reads as empty: no deletions.
    let snapshot: Snapshot = std::fs::read(&marker)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
        .unwrap_or_default();
    // Kept as `.done` rather than removed: a call that has already finished
    // must still bound the window of one that began before it.
    let done = markers.join(format!("{name}{DONE}"));
    let _ = std::fs::rename(&marker, &done);
    prune(markers, now);
    // PostToolUse runs async, so it can arrive after the next command has
    // begun — or ended. The window stops where the next one started, so that
    // command's writes are counted once, as its own.
    let next = later_start(markers, &done, start);

    let mut deleted: Vec<PathBuf> = snapshot.known.into_iter().filter(|p| !p.exists()).collect();
    deleted.extend(
        git_missing(root)
            .into_iter()
            .filter(|p| !snapshot.git_missing.contains(p)),
    );
    deleted.sort();
    deleted.dedup();

    let written = candidates(root, WALK_CAP)
        .into_iter()
        .filter(|path| {
            // Not following links: a symlink into another tree is not a file
            // this workspace's agent wrote.
            std::fs::symlink_metadata(path)
                .ok()
                .filter(|m| m.file_type().is_file())
                .and_then(|m| changed(&m))
                // Inclusive at the start: the clock files are stamped with
                // ticks every few milliseconds, so a write in the marker's own
                // tick shares its mtime.
                .is_some_and(|modified| {
                    modified >= start && modified <= now && next.is_none_or(|n| modified < n)
                })
        })
        .collect();
    Observed { written, deleted }
}

/// When a file last changed: the later of its mtime and its inode's change
/// time. `cp -p`, `tar x`, `rsync -t` and `mv` all leave the mtime in the
/// past, and only the change time records that the command touched the file.
/// The price is that `chmod` counts as a change too — it is one.
#[cfg(unix)]
fn changed(meta: &std::fs::Metadata) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt;
    let modified = meta.modified().ok()?;
    let ctime = u64::try_from(meta.ctime()).ok().map(|secs| {
        SystemTime::UNIX_EPOCH
            + std::time::Duration::new(secs, u32::try_from(meta.ctime_nsec()).unwrap_or(0))
    });
    Some(ctime.map_or(modified, |c| c.max(modified)))
}

/// Windows keeps a change time too, but std does not hand it out yet. The
/// creation time stands in: a copy that keeps its source's mtime is still a
/// new file. A file moved within a volume keeps both, and is not seen.
#[cfg(not(unix))]
fn changed(meta: &std::fs::Metadata) -> Option<SystemTime> {
    let modified = meta.modified().ok()?;
    Some(meta.created().map_or(modified, |c| c.max(modified)))
}

/// Suffix of a call that has ended, kept to bound earlier calls' windows.
const DONE: &str = ".done";

/// How long an ended call's marker is kept. Long past any async hook's
/// delay, short enough that a busy session's directory stays small.
const DONE_KEPT: std::time::Duration = std::time::Duration::from_secs(600);

/// The earliest start of any other call, open or ended, after `start`.
fn later_start(markers: &Path, own: &Path, start: SystemTime) -> Option<SystemTime> {
    std::fs::read_dir(markers)
        .ok()?
        .flatten()
        .filter(|entry| entry.path() != own)
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .filter(|&begun| begun > start)
        .min()
}

/// Drop ended calls' markers older than [`DONE_KEPT`].
fn prune(markers: &Path, now: SystemTime) {
    let Ok(entries) = std::fs::read_dir(markers) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry.file_name().to_string_lossy().ends_with(DONE)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > DONE_KEPT));
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Remove every marker a session left — a command interrupted before its
/// `PostToolUse` would otherwise outlive the session.
pub fn clear(markers: &Path) {
    let _ = std::fs::remove_dir_all(markers);
}

/// The files worth `stat`ing under `root`, at most `cap` of them.
///
/// In a repository, git's own answer: tracked and untracked files, minus
/// everything ignored — which is what keeps `target/` and `node_modules/` out
/// without thirdeye re-implementing `.gitignore`. Elsewhere, a walk that
/// skips hidden directories.
fn candidates(root: &Path, cap: usize) -> Vec<PathBuf> {
    git_files(root, cap).unwrap_or_else(|| walk(root, cap))
}

fn git_files(root: &Path, cap: usize) -> Option<Vec<PathBuf>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    // Not a repository, or no git at all: the walk is the answer.
    if !out.status.success() {
        return None;
    }
    Some(
        out.stdout
            .split(|b| *b == 0)
            .filter(|name| !name.is_empty())
            .take(cap)
            .map(|name| root.join(String::from_utf8_lossy(name).as_ref()))
            .collect(),
    )
}

fn walk(root: &Path, cap: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if found.len() >= cap {
                return found;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_ref()) {
                    pending.push(path);
                }
            } else if kind.is_file() {
                found.push(path);
            }
        }
    }
    found
}

/// A file name for a call id, which is agent-supplied: nothing in it may
/// climb out of the marker directory.
fn marker_name(call: &str) -> String {
    let mut name: String = call
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.len() > 96 {
        // Truncated, ids differing only at the end — pi's nested
        // `<parent>/1`, `<parent>/2` — would share one marker, so the name
        // keeps a hash of the whole id. FNV-1a by hand, as for workspace
        // slugs: `DefaultHasher` is not stable across releases.
        let hash = call.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x1000_0000_01b3)
        });
        name.truncate(79);
        name.push_str(&format!("-{hash:016x}"));
    }
    // "." and ".." survive the filter above and name directories.
    if name.chars().all(|c| c == '.') {
        name = format!("call{name}").replace('.', "_");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Scratch {
        root: PathBuf,
        markers: PathBuf,
    }

    fn scratch(tag: &str) -> Scratch {
        let base =
            std::env::temp_dir().join(format!("thirdeye-observe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("ws");
        std::fs::create_dir_all(&root).unwrap();
        Scratch {
            root,
            markers: base.join("s1.calls"),
        }
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// File timestamps come from the kernel's coarse clock, which ticks every
    /// few milliseconds; two writes inside one tick share an mtime.
    fn next_tick() {
        std::thread::sleep(Duration::from_millis(20));
    }

    fn git(root: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_file_written_inside_the_window_is_reported() {
        let s = scratch("inside");
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        next_tick();
        write(&s.root.join("notes.md"), "checked\n");
        let found = end(&s.markers, "exec-1", &s.root, SystemTime::now()).written;
        assert_eq!(found, vec![s.root.join("notes.md")]);
    }

    #[test]
    fn a_file_written_before_the_call_began_is_not() {
        // The lower bound is the whole difference between "this command
        // wrote it" and "this file exists".
        let s = scratch("before");
        write(&s.root.join("old.md"), "x");
        next_tick();
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        next_tick();
        assert!(
            end(&s.markers, "exec-1", &s.root, SystemTime::now())
                .written
                .is_empty()
        );
    }

    #[test]
    fn a_file_written_after_the_call_ended_is_not() {
        // With async hooks the next command may already be running when this
        // one's PostToolUse arrives; its writes are its own.
        let s = scratch("after");
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        next_tick();
        let ended = SystemTime::now();
        next_tick();
        write(&s.root.join("later.md"), "x");
        assert!(end(&s.markers, "exec-1", &s.root, ended).written.is_empty());
    }

    #[test]
    fn an_ignored_file_is_never_reported() {
        // target/ and node_modules/ are churned by every build; walking or
        // reporting them would bury the files the agent actually wrote.
        let s = scratch("ignored");
        git(&s.root, &["init", "-q"]);
        write(&s.root.join(".gitignore"), "target/\n");
        next_tick();
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        next_tick();
        write(&s.root.join("target/out.o"), "x");
        write(&s.root.join("src/lib.rs"), "x");
        let found = end(&s.markers, "exec-1", &s.root, SystemTime::now()).written;
        assert_eq!(found, vec![s.root.join("src/lib.rs")]);
    }

    #[test]
    fn outside_a_repository_hidden_directories_are_not_walked() {
        // .venv, .cache, .git: large, never what the human is looking for.
        let s = scratch("hidden");
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        next_tick();
        write(&s.root.join(".venv/lib/x.py"), "x");
        write(&s.root.join("a.txt"), "x");
        let found = end(&s.markers, "exec-1", &s.root, SystemTime::now()).written;
        assert_eq!(found, vec![s.root.join("a.txt")]);
    }

    #[test]
    fn outside_a_repository_dependency_and_build_trees_are_not_walked() {
        // `npm install` or a build in a project that is not a repository yet
        // would otherwise record thousands of writes and bury the real ones.
        let s = scratch("deps");
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        next_tick();
        write(&s.root.join("node_modules/left-pad/index.js"), "x");
        write(&s.root.join("target/debug/app"), "x");
        write(&s.root.join("__pycache__/m.pyc"), "x");
        write(&s.root.join("src/main.rs"), "x");
        let found = end(&s.markers, "exec-1", &s.root, SystemTime::now()).written;
        assert_eq!(found, vec![s.root.join("src/main.rs")]);
    }

    #[test]
    fn a_known_file_removed_inside_the_window_is_reported_deleted() {
        let s = scratch("deleted");
        let doomed = s.root.join("old.rs");
        write(&doomed, "x");
        begin(&s.markers, "exec-1", &s.root, std::slice::from_ref(&doomed)).unwrap();
        std::fs::remove_file(&doomed).unwrap();
        let seen = end(&s.markers, "exec-1", &s.root, SystemTime::now());
        assert_eq!(seen.deleted, vec![doomed]);
        assert!(seen.written.is_empty());
    }

    #[test]
    fn a_tracked_file_removed_inside_the_window_is_reported_though_never_touched() {
        let s = scratch("deleted-tracked");
        git(&s.root, &["init", "-q"]);
        write(&s.root.join("lib.rs"), "x");
        git(&s.root, &["add", "lib.rs"]);
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        std::fs::remove_file(s.root.join("lib.rs")).unwrap();
        let seen = end(&s.markers, "exec-1", &s.root, SystemTime::now());
        assert_eq!(seen.deleted, vec![s.root.join("lib.rs")]);
    }

    #[test]
    fn a_file_already_gone_before_the_call_is_not_its_deletion() {
        // Perturbation half: without the snapshot every missing tracked file
        // would be blamed on whichever command ran next.
        let s = scratch("gone-before");
        git(&s.root, &["init", "-q"]);
        write(&s.root.join("lib.rs"), "x");
        git(&s.root, &["add", "lib.rs"]);
        std::fs::remove_file(s.root.join("lib.rs")).unwrap();
        let known = s.root.join("known.rs");
        begin(&s.markers, "exec-1", &s.root, &[known]).unwrap();
        assert!(
            end(&s.markers, "exec-1", &s.root, SystemTime::now())
                .deleted
                .is_empty()
        );
    }

    #[test]
    fn only_this_workspaces_files_are_watched_for_deletion() {
        // A session can record /tmp paths; another program removing one
        // during any shell call is not this agent's deletion.
        let s = scratch("outside");
        let elsewhere = s.root.parent().unwrap().join("elsewhere.txt");
        write(&elsewhere, "x");
        let inside = s.root.join("in.rs");
        write(&inside, "x");
        let known = [elsewhere.clone(), inside.clone(), inside.clone()];
        begin(&s.markers, "exec-1", &s.root, &known).unwrap();
        std::fs::remove_file(&elsewhere).unwrap();
        std::fs::remove_file(&inside).unwrap();
        assert_eq!(
            end(&s.markers, "exec-1", &s.root, SystemTime::now()).deleted,
            vec![inside]
        );
    }

    #[test]
    fn a_file_that_still_exists_is_not_deleted() {
        let s = scratch("survivor");
        let kept = s.root.join("kept.rs");
        write(&kept, "x");
        begin(&s.markers, "exec-1", &s.root, &[kept]).unwrap();
        assert!(
            end(&s.markers, "exec-1", &s.root, SystemTime::now())
                .deleted
                .is_empty()
        );
    }

    #[test]
    fn the_walk_stops_at_its_cap() {
        // Run from a home directory, an uncapped walk would stat every file
        // the user owns on every tool call.
        let s = scratch("cap");
        for i in 0..10 {
            write(&s.root.join(format!("f{i}")), "x");
        }
        assert_eq!(candidates(&s.root, 3).len(), 3);
    }

    #[test]
    fn ending_a_call_that_never_began_reports_nothing() {
        // Hooks installed mid-command, or a PreToolUse that never ran: there
        // is no window, and guessing one would misattribute.
        let s = scratch("never");
        write(&s.root.join("a.txt"), "x");
        assert!(
            end(&s.markers, "exec-9", &s.root, SystemTime::now())
                .written
                .is_empty()
        );
    }

    #[test]
    fn a_hostile_call_id_cannot_escape_the_marker_directory() {
        // Call ids are agent-supplied, like session ids.
        let s = scratch("hostile");
        begin(&s.markers, "../../escaped", &s.root, &[]).unwrap();
        assert!(!s.markers.parent().unwrap().join("escaped").exists());
        assert_eq!(std::fs::read_dir(&s.markers).unwrap().count(), 1);
    }

    #[test]
    fn long_call_ids_that_differ_only_at_the_end_keep_distinct_windows() {
        // pi's nested calls are `<parent>/<n>`; truncated to a common prefix
        // they would share one marker, and one window would close the other.
        let parent = "call_".to_string() + &"x".repeat(100);
        let a = marker_name(&format!("{parent}/1"));
        let b = marker_name(&format!("{parent}/2"));
        assert_ne!(a, b);
        assert!(a.len() <= 96, "{}", a.len());
    }

    #[test]
    fn the_call_is_no_longer_open_after_it_ends() {
        let s = scratch("gone");
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        end(&s.markers, "exec-1", &s.root, SystemTime::now());
        assert!(!s.markers.join("exec-1").exists());
        // Ending it twice finds no window the second time.
        assert!(
            end(&s.markers, "exec-1", &s.root, SystemTime::now())
                .written
                .is_empty()
        );
    }

    #[test]
    fn a_write_that_keeps_an_old_mtime_is_still_seen() {
        // cp -p, tar x, rsync -t and mv all leave the mtime in the past; the
        // inode's change time still says when it happened.
        let s = scratch("old-mtime");
        let elsewhere = s.root.parent().unwrap().join("archive.txt");
        write(&elsewhere, "x");
        next_tick();
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        next_tick();
        write(&s.root.join("extracted.txt"), "x");
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(s.root.join("extracted.txt"))
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        std::fs::rename(&elsewhere, s.root.join("moved.txt")).unwrap();
        let mut found = end(&s.markers, "exec-1", &s.root, SystemTime::now()).written;
        found.sort();
        let mut expected = vec![s.root.join("extracted.txt")];
        // Windows keeps both of a moved file's times: see `changed`.
        if cfg!(unix) {
            expected.push(s.root.join("moved.txt"));
        }
        assert_eq!(found, expected);
    }

    #[test]
    fn ended_calls_are_forgotten_after_a_while() {
        // One marker per tool call; a long session must not accumulate them.
        let s = scratch("prune");
        begin(&s.markers, "old", &s.root, &[]).unwrap();
        end(&s.markers, "old", &s.root, SystemTime::now());
        let old = s.markers.join("old.done");
        let hour_ago = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        begin(&s.markers, "new", &s.root, &[]).unwrap();
        end(&s.markers, "new", &s.root, SystemTime::now());
        assert!(!old.exists());
        assert!(s.markers.join("new.done").exists());
    }

    #[test]
    fn a_later_calls_writes_are_not_counted_against_an_earlier_one() {
        // PostToolUse runs async, so A's can arrive after B has begun —
        // or even ended. B's writes are B's, not A's as well.
        for b_ends_first in [true, false] {
            let s = scratch(if b_ends_first {
                "later-ended"
            } else {
                "later-open"
            });
            begin(&s.markers, "A", &s.root, &[]).unwrap();
            next_tick();
            write(&s.root.join("a.txt"), "x");
            next_tick();
            begin(&s.markers, "B", &s.root, &[]).unwrap();
            next_tick();
            write(&s.root.join("b.txt"), "x");
            if b_ends_first {
                let b = end(&s.markers, "B", &s.root, SystemTime::now()).written;
                assert_eq!(b, vec![s.root.join("b.txt")]);
            }
            let a = end(&s.markers, "A", &s.root, SystemTime::now()).written;
            assert_eq!(a, vec![s.root.join("a.txt")], "b_ends_first={b_ends_first}");
        }
    }

    #[test]
    fn clearing_removes_every_marker_a_session_left() {
        // An interrupted command never sends PostToolUse; its marker would
        // otherwise outlive the session.
        let s = scratch("clear");
        begin(&s.markers, "exec-1", &s.root, &[]).unwrap();
        clear(&s.markers);
        assert!(!s.markers.exists());
    }
}
