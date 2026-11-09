//! The turn an agent without turn ids is in, kept between hook invocations.
//!
//! Copilot's events name no turn, but its hooks are synchronous, so they
//! arrive in order and "the turn opened by the last prompt" is well defined.
//! One file beside the session's socket holds its id. Hook-side scratch, like
//! the observe markers: every id it hands out is written to the log, so the
//! daemon still derives everything from the log alone.

use std::path::{Path, PathBuf};

pub fn path_for(socket: &Path) -> PathBuf {
    socket.with_extension("turn")
}

/// Hold `id` as the open turn, replacing one a missed stop left open.
pub fn open(path: &Path, id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, id)
}

pub fn current(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .filter(|id| !id.is_empty())
}

/// The open turn, now closed.
pub fn close(path: &Path) -> Option<String> {
    let id = current(path);
    let _ = std::fs::remove_file(path);
    id
}
