//! The socket a daemon and its clients meet at, on every platform.
//!
//! A Unix socket everywhere: Windows has had `AF_UNIX` since 1803, so the
//! path, the pointer beside it and the protocol are the same there. Only the
//! type differs — std's own is still unstable on Windows — and this is the one
//! place that says which.

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};
