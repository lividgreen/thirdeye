//! The ratatui viewer — `thirdeye head`.
//!
//! Thin by design. It holds no model of its own beyond what the daemon sends,
//! reads no files (the daemon serves content), and decides only presentation
//! and key mapping. Everything interesting is in [`state`], which knows
//! nothing about terminals, and [`render`], which draws into any backend.
//!
//! This module is the part that cannot be unit-tested: raw mode, the alternate
//! screen, and a socket. It is kept correspondingly small.

pub mod render;
pub mod state;

use crate::local_socket::UnixStream;
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use thirdeye_core::EventPaths;

use crate::daemon::protocol::{ClientMessage, HideUntil, PROTOCOL_VERSION, Role, ServerMessage};
use crate::daemon::transport::{IoTransport, Transport};
use crate::session_ref;
use crate::workspace;
use state::{HeadState, Key};

/// How long to wait for a key before looking at the socket again.
///
/// The head is idle almost always; this is the cost of noticing a pushed
/// patch, and 100ms is well under the threshold where a human calls a redraw
/// "late".
const TICK: Duration = Duration::from_millis(100);

pub fn run(
    session: String,
    explicit_workspace: Option<PathBuf>,
    launched_by_surface: bool,
) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    let ws = workspace::resolve(explicit_workspace.clone(), cwd.as_deref());
    let paths = EventPaths::discover(ws);
    let socket = paths
        .socket_path(&session)
        .context("no socket path fits for this session")?;

    // The same pointer the daemon uses. A head launched by a surface has the
    // instance name and little else, exactly like the daemon does.
    let session = match session_ref::load(&socket) {
        Some(found) if explicit_workspace.is_none() => found.session,
        _ => session,
    };

    let stream = UnixStream::connect(&socket).with_context(|| {
        format!(
            "no daemon is listening at {} — start a session, or run `thirdeye daemon --session {session}`",
            socket.display()
        )
    })?;

    let messages = spawn_reader(stream.try_clone().context("cloning the connection")?);
    let mut out = IoTransport::new(std::io::empty(), stream);
    send(
        &mut out,
        &ClientMessage::Hello {
            role: Role::Head,
            protocol_version: PROTOCOL_VERSION,
            head_id: Some("tui".into()),
            launched_by_surface,
        },
    )?;

    let mut terminal = enter()?;
    let result = event_loop(&mut terminal, &mut out, &messages);
    // Restoring the terminal matters more than the error that got us here: a
    // head that dies in raw mode leaves the human's shell unusable.
    let restored = leave(&mut terminal);
    result.and(restored)
}

/// Read messages off the socket on their own thread.
///
/// The alternative — polling the socket from the draw loop — means either a
/// blocking read that freezes the UI or a non-blocking one that spins.
fn spawn_reader(stream: UnixStream) -> Receiver<ServerMessage> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut transport = IoTransport::new(stream, std::io::sink());
        while let Ok(Some(line)) = transport.recv_line() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ServerMessage>(&line) {
                // A message this build does not understand costs the message,
                // not the session: an older head should lose a feature rather
                // than the connection.
                Err(_) => continue,
                Ok(message) => {
                    if tx.send(message).is_err() {
                        return; // The UI is gone.
                    }
                }
            }
        }
    });
    rx
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn event_loop(
    terminal: &mut Tui,
    out: &mut impl Transport,
    messages: &Receiver<ServerMessage>,
) -> Result<()> {
    let mut state = HeadState::new();

    loop {
        terminal.draw(|frame| render::draw(&state, frame))?;

        // Everything the daemon has said since the last frame, then one key.
        // Draining first keeps a burst of patches from being rendered one
        // frame at a time.
        loop {
            match messages.try_recv() {
                Ok(message) => {
                    for reply in state.on_message(message) {
                        send(out, &reply)?;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // The reader thread ends only when the socket does.
                Err(TryRecvError::Disconnected) => {
                    state.on_daemon_gone();
                    break;
                }
            }
        }
        if state.should_quit {
            return Ok(());
        }

        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            // Windows reports press *and* release; without this every key
            // would act twice.
            && key.kind == KeyEventKind::Press
            && let Some(key) = translate(key.code, key.modifiers)
        {
            for reply in state.on_key(key) {
                send(out, &reply)?;
            }
            if state.should_quit {
                return Ok(());
            }
        }
    }
}

/// Key mapping is the head's business, not the daemon's.
fn translate(code: KeyCode, modifiers: KeyModifiers) -> Option<Key> {
    if modifiers.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        return Some(Key::Quit);
    }
    match code {
        KeyCode::Up | KeyCode::Char('k') => Some(Key::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Key::Down),
        KeyCode::Enter | KeyCode::Char(' ') => Some(Key::Activate),
        KeyCode::Esc => Some(Key::Back),
        KeyCode::Char('d') => Some(Key::ToggleDiff),
        // Three keys rather than one key and a question: the human closing
        // the viewer usually knows exactly how long they want it gone.
        KeyCode::Char('q') => Some(Key::Hide(HideUntil::NextTurn)),
        KeyCode::Char('m') => Some(Key::Hide(HideUntil::NextMutation)),
        KeyCode::Char('Q') => Some(Key::Hide(HideUntil::Session)),
        _ => None,
    }
}

fn send(out: &mut impl Transport, message: &ClientMessage) -> Result<()> {
    out.send_line(&serde_json::to_string(message)?)
}

fn enter() -> Result<Tui> {
    enable_raw_mode().context("entering raw mode")?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen).context("entering the alternate screen")?;
    Terminal::new(CrosstermBackend::new(stdout)).context("starting the terminal")
}

fn leave(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode().context("leaving raw mode")?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("leaving the alternate screen")?;
    terminal.show_cursor().context("restoring the cursor")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_a_human_expects_are_mapped() {
        let none = KeyModifiers::NONE;
        assert_eq!(translate(KeyCode::Char('j'), none), Some(Key::Down));
        assert_eq!(translate(KeyCode::Down, none), Some(Key::Down));
        assert_eq!(translate(KeyCode::Char('k'), none), Some(Key::Up));
        assert_eq!(translate(KeyCode::Enter, none), Some(Key::Activate));
        assert_eq!(translate(KeyCode::Esc, none), Some(Key::Back));
        assert_eq!(translate(KeyCode::Char('d'), none), Some(Key::ToggleDiff));
    }

    #[test]
    fn each_way_of_closing_the_viewer_has_its_own_key() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            translate(KeyCode::Char('q'), none),
            Some(Key::Hide(HideUntil::NextTurn))
        );
        assert_eq!(
            translate(KeyCode::Char('m'), none),
            Some(Key::Hide(HideUntil::NextMutation))
        );
        // Terminals report the shifted letter with SHIFT set; the letter alone
        // must decide, or `Q` would do nothing on most of them.
        assert_eq!(
            translate(KeyCode::Char('Q'), KeyModifiers::SHIFT),
            Some(Key::Hide(HideUntil::Session))
        );
    }

    #[test]
    fn ctrl_c_quits_even_though_raw_mode_swallows_it() {
        // In raw mode the terminal does not send SIGINT, so a head that did
        // not handle this itself could only be killed from another window.
        assert_eq!(
            translate(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Key::Quit)
        );
    }

    #[test]
    fn unmapped_keys_do_nothing_rather_than_something_surprising() {
        assert_eq!(translate(KeyCode::Char('x'), KeyModifiers::NONE), None);
        assert_eq!(translate(KeyCode::Tab, KeyModifiers::NONE), None);
    }
}
