//! Viewers. One of the five seams (§5).
//!
//! A head is **whatever speaks the socket protocol** — it is not a Rust trait,
//! and it need not live in this crate or be written in this language. That is
//! the point of the protocol being newline-delimited JSON: a browser head
//! talking over a websocket is a different program, not a different `impl`.
//!
//! So this directory holds the heads that happen to ship *with* thirdeye, one
//! per subdirectory named for what it is. `tui` is the ratatui viewer; a `gui`
//! would be a sibling. The seam itself lives in `daemon::protocol`.
//!
//! Head and Surface are separate axes: kitty is *where* a head is displayed,
//! a TUI is *what* the head is, and a GUI head makes its own window and needs
//! no surface at all.

pub mod tui;
