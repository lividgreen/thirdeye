//! Carrying messages, without caring what carries them.
//!
//! One of the five seams (§5). The PoC speaks over a unix socket, but the
//! protocol is newline-delimited JSON precisely so that it does not depend on
//! one: a named pipe, loopback TCP, or a websocket to a browser head all work
//! unchanged.
//!
//! The immediate reason this is a trait is §14 — "protocol gets round-trip
//! tests over an in-memory transport, decoupled from unix sockets". A protocol
//! whose tests need a socket is a protocol whose failures are timeouts.
//!
//! **One implementation per file.** `mod.rs` holds the seam — the trait and
//! nothing else — so adding a transport is adding a file and one `pub mod`
//! line, never editing someone else's implementation.

pub mod io;
#[cfg(test)]
pub mod memory;

use anyhow::Result;

pub use io::IoTransport;

/// A bidirectional, line-delimited message channel.
pub trait Transport {
    /// Send one message. Implementations must flush: the peer is blocking on
    /// this, and a buffered protocol message is a hang, not a slowdown.
    fn send_line(&mut self, line: &str) -> Result<()>;

    /// Receive one message, or `None` when the peer has hung up.
    fn recv_line(&mut self) -> Result<Option<String>>;
}
