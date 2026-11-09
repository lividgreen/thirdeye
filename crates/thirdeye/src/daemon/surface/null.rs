//! A surface that displays nothing and records every decision.
//!
//! Not a mock for one test: §14 puts every policy and visibility transition
//! behind a scripted log plus this type, so none of that logic needs a
//! terminal to be exercised. It is the reason the daemon's decisions are
//! separable from the one part of the system that cannot run in CI.

use anyhow::Result;

use super::{Surface, ViewRequest};

#[derive(Debug, Default)]
pub struct NullSurface {
    open: bool,
    /// Every transition, in order, for tests to assert against.
    pub log: Vec<SurfaceEvent>,
    /// Stand in for a window-opening surface, to exercise what the daemon
    /// does differently for one.
    detached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceEvent {
    Opened(ViewRequest),
    Closed,
}

impl Surface for NullSurface {
    fn id(&self) -> &'static str {
        "none"
    }

    fn available(&self) -> bool {
        true
    }

    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        // Idempotence is asserted here rather than left to callers: the
        // visibility rules fire on every event, so `open` is called far more
        // often than it transitions.
        if !self.open {
            self.open = true;
            self.log.push(SurfaceEvent::Opened(request.clone()));
        }
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        if self.open {
            self.open = false;
            self.log.push(SurfaceEvent::Closed);
        }
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.open
    }

    fn detached(&self) -> bool {
        self.detached
    }
}

impl NullSurface {
    /// Test-only: a stand-in for a surface that opens windows.
    #[cfg(test)]
    pub fn detached() -> Self {
        Self {
            detached: true,
            ..Self::default()
        }
    }

    /// Test-only: counting transitions is how the visibility rules are
    /// asserted, and nothing in production asks a surface what it has done.
    #[cfg(test)]
    pub fn opens(&self) -> usize {
        self.log
            .iter()
            .filter(|e| matches!(e, SurfaceEvent::Opened(_)))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ViewRequest {
        ViewRequest {
            session: "s1".into(),
            workspace: "/w".into(),
            head: "thirdeye".into(),
        }
    }

    #[test]
    fn opening_twice_shows_one_head() {
        let mut s = NullSurface::default();
        s.open(&request()).unwrap();
        s.open(&request()).unwrap();
        assert_eq!(s.opens(), 1);
        assert!(s.is_open());
    }

    #[test]
    fn closing_something_never_opened_is_not_an_error() {
        // The daemon closes on SessionEnd regardless of what it opened, and a
        // failure there would surface as a broken teardown for no reason.
        let mut s = NullSurface::default();
        s.close().unwrap();
        assert!(
            s.log.is_empty(),
            "a no-op close must not record a transition"
        );
    }

    #[test]
    fn reopening_after_a_close_is_a_new_transition() {
        let mut s = NullSurface::default();
        s.open(&request()).unwrap();
        s.close().unwrap();
        s.open(&request()).unwrap();
        assert_eq!(s.opens(), 2);
        assert_eq!(
            s.log,
            vec![
                SurfaceEvent::Opened(request()),
                SurfaceEvent::Closed,
                SurfaceEvent::Opened(request()),
            ]
        );
    }
}
