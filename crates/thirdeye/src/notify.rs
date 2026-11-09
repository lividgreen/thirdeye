//! The hook's second job: telling a daemon that something changed.
//!
//! **Advisory, never the data path.** The append has already happened and is
//! durable by the time any of this runs; the connection carries a `Hello` and
//! nothing else, and the daemon reads the log to learn what actually changed.
//! Every failure here is therefore latency-only and must stay silent — this
//! runs on every tool call, and a hook that fails loudly would surface inside
//! the user's agent session.

use crate::local_socket::UnixStream;
use std::io::Write;
use std::path::Path;
#[cfg(unix)]
use std::process::{Command, Stdio};

use crate::daemon::protocol::ClientMessage;

/// Notify the daemon for `session`, starting it if it is not running.
///
/// Returns quietly on every failure. The caller is a hook whose exit code the
/// wrapper script discards anyway; the point of returning `()` is to make it
/// impossible for a future caller to start treating this as fallible work.
pub fn poke(socket: &Path) {
    #[cfg(unix)]
    poke_with(socket, start_socket_unit);
    #[cfg(windows)]
    poke_with(socket, start_daemon);
}

/// [`poke`] with unit-starting injected.
///
/// Tests must never actually run `systemctl`: it would touch the developer's
/// own user manager and leave failed units behind for a unit file that is not
/// even installed. Injecting keeps every test below hermetic.
fn poke_with(socket: &Path, start: impl Fn(&Path) -> std::io::Result<()>) {
    if try_connect(socket).is_ok() {
        return;
    }

    // ENOENT means the socket unit is not started — the first tool call of a
    // session, or after a `SessionEnd`. Start it and retry exactly once.
    // Once, because the event is already on disk: a daemon that comes up a
    // moment later replays the log and reaches the same state, so retrying
    // harder buys nothing and delays the agent. That is all a daemon started
    // here rather than by systemd ever gets: it is not listening yet.
    if start(socket).is_ok() {
        let _ = try_connect(socket);
    }
}

/// Stop the session's daemon: under systemd its socket unit, which stops
/// it; elsewhere by telling it the session is over.
///
/// Called when the agent's session ends. Without it the daemon lingers until
/// logout — and now that it drives a surface, so does the split it opened,
/// left pointing at a session nobody is working in.
///
/// Silent and best-effort like everything else here: a session that has ended
/// is not a moment to start reporting errors into.
pub fn stop(socket: &Path) {
    #[cfg(unix)]
    {
        let Some(instance) = instance_of(socket) else {
            return;
        };
        let _ = Command::new("systemctl")
            .args(stop_args(instance))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(windows)]
    {
        let _ = end(socket);
        forget(socket);
    }
}

/// Remove what a daemon leaves beside its socket. Nothing clears this
/// directory at logout, as a runtime directory is cleared: it would all
/// stay, a set for every session there has been.
#[cfg(windows)]
pub fn forget(socket: &Path) {
    for left in ["sock", "lock", "log"] {
        let _ = std::fs::remove_file(socket.with_extension(left));
    }
}

/// How long a session's end waits for its daemon to act on it. Codex gives
/// `SessionEnd` one to three seconds in all.
#[cfg(any(windows, test))]
const END_WAIT: std::time::Duration = std::time::Duration::from_millis(800);

/// Tell a daemon that no service manager runs that its session is over.
///
/// Waits for it to hang up, briefly: a connection closed with the daemon's
/// reply unread can take the request down with it, and the daemon closes
/// the view before it answers.
#[cfg(any(windows, test))]
fn end(socket: &Path) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    for message in [ClientMessage::end_hello(), ClientMessage::End] {
        let encoded = serde_json::to_string(&message)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(stream, "{encoded}")?;
    }
    stream.set_read_timeout(Some(END_WAIT))?;
    let _ = std::io::copy(&mut stream, &mut std::io::sink());
    Ok(())
}

/// `--no-block`: queue the stop and return. Codex runs `SessionEnd`
/// synchronously and kills it after one to three seconds; a stop that waited
/// for the unit and overran would be killed with the daemon still running.
#[cfg(any(unix, test))]
fn stop_args(instance: &str) -> Vec<String> {
    vec![
        "--user".into(),
        "--no-block".into(),
        "stop".into(),
        format!("thirdeye@{instance}.socket"),
        format!("thirdeye@{instance}.service"),
    ]
}

fn try_connect(socket: &Path) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    let hello = serde_json::to_string(&ClientMessage::hook_hello())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(stream, "{hello}")
}

/// Start the daemon ourselves, where no service manager can.
///
/// In two steps. A process std starts inherits every handle its parent can
/// pass on, the hook's own pipes among them, and whoever ran the hook waits
/// on those: a daemon holding them made a hook that had long exited look as
/// if it were still running. So the hook starts `daemon --detach` with
/// nothing inherited — which std cannot ask for, and `winsafe` can — and
/// that one, with no pipe to pass on, starts the daemon.
///
/// No console of its own to flash up, but a hidden one, so that the `git`
/// and terminal commands the daemon runs do not each open theirs.
///
/// Every hook that finds no daemon starts one; `SelfBind::claim` lets one
/// of them stay.
#[cfg(windows)]
fn start_daemon(socket: &Path) -> std::io::Result<()> {
    use winsafe::{self as w, co};

    let invalid = |why: &str| std::io::Error::new(std::io::ErrorKind::InvalidInput, why);
    // Both go into a command line written by hand. The stem is what
    // `sanitize_component` left; a path cannot hold a quote.
    let instance = instance_of(socket)
        .filter(|stem| {
            stem.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        })
        .ok_or_else(|| invalid("socket path has no usable stem"))?;
    // From this build's copy, not the package manager's file: the daemon
    // lives all session, and a running exe cannot be upgraded in place.
    let current = std::env::current_exe()?;
    let exe = thirdeye_core::location::data_root()
        .map(|data| crate::shadow::run_path(&data, &current))
        .unwrap_or(current);
    let exe = exe
        .to_str()
        .filter(|path| !path.contains('"'))
        .ok_or_else(|| invalid("this program's path cannot be quoted"))?;

    w::CreateProcess(
        None,
        Some(&format!("\"{exe}\" daemon --session {instance} --detach")),
        None,
        None,
        false,
        co::CREATE::NEW_PROCESS_GROUP | co::CREATE::NO_WINDOW,
        &[],
        None,
        &mut w::STARTUPINFO::default(),
    )
    .map(drop)
    .map_err(|e| std::io::Error::other(e.to_string()))
}

/// `systemctl --user start <instance>.socket`.
///
/// The instance name is the socket's own file stem, not the raw session id.
/// Both come from `sanitize_component`, which emits only `[A-Za-z0-9._-]` —
/// already a valid systemd instance name — so no `systemd-escape` subprocess
/// is needed on a path that runs for every tool call. It also guarantees the
/// unit's `ListenStream=%t/thirdeye/%i.sock` names the very socket the hook
/// just failed to reach, rather than a path derived a second, divergent way.
#[cfg(unix)]
fn start_socket_unit(socket: &Path) -> std::io::Result<()> {
    let Some(instance) = instance_of(socket) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socket path has no usable stem",
        ));
    };

    let status = Command::new("systemctl")
        .args(["--user", "start", &format!("thirdeye@{instance}.socket")])
        // systemctl's own output must not reach the agent session.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "systemctl exited with {status}"
        )))
    }
}

/// The systemd instance name for a session's socket: the socket's own stem.
/// What a daemon started by hand, or by a hook, is given as its session too.
fn instance_of(socket: &Path) -> Option<&str> {
    socket.file_stem().and_then(|s| s.to_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_socket::UnixListener;

    #[test]
    fn stopping_does_not_wait_for_the_unit() {
        // Codex runs SessionEnd synchronously and kills it after 1-3 s. A
        // blocking stop that overran would leave the daemon running.
        assert!(stop_args("s1").iter().any(|a| a == "--no-block"));
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("thirdeye-notify-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A starter that records the attempt instead of touching systemd.
    fn refuses(_: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("no unit manager in tests"))
    }

    #[test]
    fn a_listening_daemon_receives_one_hello_line() {
        let socket = tmp("hello").join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        poke_with(&socket, refuses);

        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stream), &mut line).unwrap();

        let hello: ClientMessage = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(hello, ClientMessage::hook_hello());
        assert!(line.ends_with('\n'), "the transport is line-delimited");
    }

    #[test]
    fn ending_a_session_is_said_as_a_head_and_waits_for_the_hangup() {
        // A hook's hello is answered by a refresh and nothing after it is
        // read, so the end of a session greets as a head does. And it stays
        // until the daemon hangs up: that is the daemon having acted.
        use std::io::BufRead;
        let socket = tmp("end").join("d.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let ending = std::thread::spawn({
            let socket = socket.clone();
            move || end(&socket)
        });

        let (stream, _) = listener.accept().unwrap();
        let mut lines = std::io::BufReader::new(stream).lines();
        let mut next =
            || -> ClientMessage { serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap() };
        assert_eq!(next(), ClientMessage::end_hello());
        assert_eq!(next(), ClientMessage::End);
        assert!(!ending.is_finished(), "left before the daemon answered");
        drop(lines);
        ending.join().unwrap().unwrap();
    }

    #[test]
    fn a_missing_daemon_never_makes_the_hook_fail() {
        // The whole contract of this module. If this ever panics or hangs,
        // every tool call in the user's session pays for it.
        let socket = tmp("absent").join("nothing-here.sock");
        poke_with(&socket, refuses);
    }

    #[test]
    fn a_started_unit_gets_exactly_one_retry() {
        // Not two, and not a loop: the event is already durable, so a daemon
        // that comes up late replays the log to the same state. Retrying
        // harder would only make the agent wait.
        use std::sync::atomic::{AtomicUsize, Ordering};
        static STARTS: AtomicUsize = AtomicUsize::new(0);

        let socket = tmp("retry").join("d.sock");
        poke_with(&socket, |_| {
            STARTS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(STARTS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_dead_socket_file_never_makes_the_hook_fail() {
        // A file at the path that is not a socket: what a crashed daemon
        // leaves behind. connect() fails with ECONNREFUSED, not ENOENT.
        let socket = tmp("dead").join("d.sock");
        std::fs::write(&socket, b"not a socket").unwrap();
        poke_with(&socket, refuses);
    }
}
