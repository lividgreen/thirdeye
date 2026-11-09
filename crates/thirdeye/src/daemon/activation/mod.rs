//! How the daemon comes to have a listening socket.
//!
//! One of the five seams (§5). Under systemd the socket is created by the
//! *socket unit* and handed over already listening, which is what lets a hook
//! start the daemon by connecting to it. Everywhere else — tests, containers,
//! Alpine, WSL without systemd — the daemon binds its own.
//!
//! **One implementation per file.** `mod.rs` holds the seam and nothing else,
//! so adding launchd or a self-spawning lock is adding a file and one
//! `pub mod` line rather than editing an implementation someone relies on.

pub mod self_bind;
#[cfg(unix)]
pub mod systemd;

use crate::local_socket::UnixListener;

use anyhow::Result;

pub use self_bind::SelfBind;
#[cfg(unix)]
pub use systemd::{SocketActivated, notify_ready, socket_activated};

/// A source of the daemon's listening socket.
pub trait Activation {
    fn listener(&self) -> Result<UnixListener>;
}
