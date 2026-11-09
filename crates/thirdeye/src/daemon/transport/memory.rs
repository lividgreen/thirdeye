//! An in-process transport pair, for tests.
//!
//! Not a mock: it implements the same trait, so a protocol bug surfaces here
//! rather than hiding behind a socket that happens to work. §14 asks for the
//! protocol to be testable without sockets, and this is that.

use anyhow::Result;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::Transport;

type Queue = Arc<Mutex<VecDeque<String>>>;

/// One end of a pair. Whatever `a` sends, `b` receives.
pub struct MemoryTransport {
    outbound: Queue,
    inbound: Queue,
    /// Set when the peer is dropped, so `recv_line` can report hangup
    /// rather than block forever on an empty queue.
    closed: Arc<Mutex<bool>>,
}

pub fn pair() -> (MemoryTransport, MemoryTransport) {
    let (one, two) = (Queue::default(), Queue::default());
    let closed = Arc::new(Mutex::new(false));
    (
        MemoryTransport {
            outbound: one.clone(),
            inbound: two.clone(),
            closed: closed.clone(),
        },
        MemoryTransport {
            outbound: two,
            inbound: one,
            closed,
        },
    )
}

impl MemoryTransport {
    pub fn hang_up(&self) {
        *self.closed.lock().unwrap() = true;
    }
}

impl Transport for MemoryTransport {
    fn send_line(&mut self, line: &str) -> Result<()> {
        assert!(
            !line.contains('\n'),
            "a message containing a newline would be read as two: {line:?}"
        );
        self.outbound.lock().unwrap().push_back(line.to_string());
        Ok(())
    }

    fn recv_line(&mut self) -> Result<Option<String>> {
        Ok(self.inbound.lock().unwrap().pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_survives_the_round_trip() {
        let (mut a, mut b) = pair();
        a.send_line("hello").unwrap();
        assert_eq!(b.recv_line().unwrap().as_deref(), Some("hello"));
    }

    #[test]
    fn an_empty_channel_reports_nothing_rather_than_blocking() {
        let (_a, mut b) = pair();
        assert_eq!(b.recv_line().unwrap(), None);
    }

    #[test]
    fn messages_arrive_in_the_order_they_were_sent() {
        // A Snapshot followed by a Patch that assumes it is not the same as
        // the reverse, so ordering is part of the contract, not a detail.
        let (mut a, mut b) = pair();
        for m in ["first", "second", "third"] {
            a.send_line(m).unwrap();
        }
        let got: Vec<_> = std::iter::from_fn(|| b.recv_line().unwrap()).collect();
        assert_eq!(got, vec!["first", "second", "third"]);
    }
}
