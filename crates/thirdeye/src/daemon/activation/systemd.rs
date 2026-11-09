//! Socket activation, and telling systemd we are up.
//!
//! Everything here is systemd-specific and lives together for that reason: a
//! launchd or self-spawn implementation shares the trait, not this file.

use std::os::unix::net::UnixListener;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::Activation;

/// Take the listener systemd already created and passed in.
///
/// This is the whole point of socket activation: the *socket unit* owns the
/// socket, so a hook connecting to it starts the service. The daemon inherits
/// a listener that was already accepting connections before the process
/// existed, which is why there is no window in which a notification is lost.
///
/// The inherited descriptor is fd 3 by convention (`LISTEN_FDS`), and turning
/// a raw descriptor into a `UnixListener` is the one `unsafe` operation this
/// design needs. `listenfd` contains it, so the workspace keeps
/// `forbid(unsafe_code)` with no exceptions in code we write — see D1 of
/// `docs/design/plans/2026-08-01-daemon-skeleton.md`.
#[derive(Debug, Default)]
pub struct SocketActivated;

impl Activation for SocketActivated {
    fn listener(&self) -> Result<UnixListener> {
        let mut fds = listenfd::ListenFd::from_env();
        fds.take_unix_listener(0)
            .context("taking the socket systemd passed in")?
            // Reached when the daemon was started directly rather than by the
            // socket unit — `systemctl start thirdeye@x.service` instead of
            // `.socket`, or a hand-run binary. Saying so beats hanging with no
            // listener, which is what a silent fallback would look like.
            .context("no socket was passed in: not started by a socket unit")
    }
}

/// Whether this process was started by a systemd socket unit.
///
/// Checked rather than assumed so `thirdeye daemon --session x` still works
/// when run by hand — the same binary has to serve both, and a daemon that
/// only worked under systemd could not be debugged.
pub fn socket_activated() -> bool {
    // `listenfd` validates LISTEN_PID against our own pid; this is only the
    // cheap pre-check for which strategy to build.
    std::env::var_os("LISTEN_FDS").is_some()
}

/// Tell systemd the daemon is up, if it is listening for that.
///
/// `Type=notify` makes `systemctl start` block until this arrives, which is
/// what keeps the journal ordered and makes a failed start actually report as
/// failed. Hand-rolled rather than taking a second dependency: the protocol is
/// one datagram, and `UnixDatagram` covers both the path and abstract-socket
/// forms with no unsafe.
///
/// A missing `NOTIFY_SOCKET` means nobody is listening — the normal case when
/// run by hand — and is not an error.
pub fn notify_ready() -> Result<()> {
    let Some(target) = std::env::var_os("NOTIFY_SOCKET") else {
        return Ok(());
    };
    let target = PathBuf::from(target);
    let socket = std::os::unix::net::UnixDatagram::unbound()
        .context("creating the notify datagram socket")?;

    let raw = target.as_os_str().as_encoded_bytes();
    if raw.first() == Some(&b'@') {
        // Abstract namespace: systemd spells the leading NUL as '@'.
        #[cfg(target_os = "linux")]
        {
            use std::os::linux::net::SocketAddrExt;
            let addr = std::os::unix::net::SocketAddr::from_abstract_name(&raw[1..])
                .context("parsing an abstract NOTIFY_SOCKET name")?;
            socket
                .send_to_addr(b"READY=1", &addr)
                .context("notifying systemd over an abstract socket")?;
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("abstract sockets are Linux-only, but NOTIFY_SOCKET names one");
    }
    socket
        .send_to(b"READY=1", &target)
        .with_context(|| format!("notifying systemd at {}", target.display()))?;
    Ok(())
}
