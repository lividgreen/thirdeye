//! Binding our own socket — the portable path.
//!
//! What every test uses, and what runs anywhere without a service manager.
//! Under systemd this is *not* what runs: the socket unit owns the socket so a
//! connection can start the service, and a daemon binding its own would race
//! the unit for the same path.

use crate::local_socket::UnixListener;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::Activation;

/// Bind the socket ourselves.
///
/// The portable path, and the one every test uses. Under systemd this is *not*
/// what runs — the socket unit owns the socket so that a connection can start
/// the service, and a daemon that bound its own would race the unit for the
/// same path.
#[derive(Debug, Clone)]
pub struct SelfBind {
    path: PathBuf,
}

impl SelfBind {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Become the one daemon for this socket, or learn that another is.
    ///
    /// Where a hook starts the daemon itself, several hooks of one turn each
    /// start one, and binding removes whatever file is at the path — so the
    /// last to start would take the socket from under the others and leave
    /// them running, unreachable. An exclusive lock on a file beside the
    /// socket decides instead, and the operating system releases it however
    /// the holder dies. `None` means another daemon holds it.
    ///
    /// The lock lasts as long as the returned file is kept.
    pub fn claim(&self) -> Result<Option<std::fs::File>> {
        create_private_dir(&self.path)?;
        let path = self.path.with_extension("lock");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("locking {}", path.display()))
            }
        }
    }
}

/// The socket's directory, `0700`.
///
/// The directory listing is the set of live session ids, and the default
/// umask would publish it to every local user. systemd creates this directory
/// itself as 0755 when it owns the socket — recorded as a known gap in the
/// daemon-skeleton plan, and not exploitable, since the sockets themselves
/// are 0600 either way.
fn create_private_dir(socket: &Path) -> Result<()> {
    let Some(parent) = socket.parent() else {
        return Ok(());
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(parent)
        .or_else(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => Ok(()),
            _ => Err(e),
        })
        .with_context(|| format!("creating socket directory {}", parent.display()))
}

impl Activation for SelfBind {
    fn listener(&self) -> Result<UnixListener> {
        create_private_dir(&self.path)?;
        // A socket file left by a previous run is not a live listener — bind
        // fails with EADDRINUSE on the stale path and the daemon would never
        // start again after a crash. Removing it is safe for the daemon that
        // holds the claim: no other is listening there.
        remove_stale(&self.path)?;

        let listener = UnixListener::bind(&self.path)
            .with_context(|| format!("binding {}", self.path.display()))?;
        restrict_socket(&self.path)?;
        Ok(listener)
    }
}

fn remove_stale(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing stale socket {}", path.display())),
    }
}

/// `0600` on the socket, matching the spec's requirement for the runtime dir.
///
/// The socket is a control channel: anything that can connect can drive the
/// human's terminal. The same tolerance as the event log applies — a
/// filesystem with no permission bits must not stop the daemon starting.
fn restrict_socket(path: &Path) -> Result<()> {
    // Windows has no mode bits to set: the socket is as private as the
    // directory it is in, which is inside the user's own profile.
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            Err(e)
                if !matches!(
                    e.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
                ) =>
            {
                return Err(e).with_context(|| format!("securing {}", path.display()));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thirdeye-act-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn binding_creates_a_listening_socket() {
        let path = tmp("bind").join("d.sock");
        let listener = SelfBind::new(&path).listener().unwrap();
        assert!(path.exists());
        // A bound listener accepts: connect from this process and see it.
        let _client = crate::local_socket::UnixStream::connect(&path).unwrap();
        assert!(listener.accept().is_ok());
    }

    #[test]
    fn a_socket_left_by_a_crashed_daemon_does_not_block_the_next_start() {
        // Without this the first crash would make the daemon unstartable
        // until someone deleted the file by hand, and the failure would look
        // like "address already in use" for an address nothing is listening on.
        let path = tmp("stale").join("d.sock");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not a socket").unwrap();

        let listener = SelfBind::new(&path).listener().unwrap();
        let _client = crate::local_socket::UnixStream::connect(&path).unwrap();
        assert!(listener.accept().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_is_reachable_only_by_its_owner() {
        // Anything that can connect can drive the human's terminal.
        use std::os::unix::fs::PermissionsExt;
        let path = tmp("perms").join("d.sock");
        let _listener = SelfBind::new(&path).listener().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "got {:o}", mode & 0o777);
    }
}
