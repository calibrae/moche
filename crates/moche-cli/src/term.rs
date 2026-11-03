//! Raw mode, window size and terminal restoration.

use std::io::Write;
use std::os::fd::AsFd;
use std::sync::Mutex;

use rustix::termios::{self, OptionalActions, Termios};

struct Saved {
    termios: Termios,
    altscreen: bool,
}

static SAVED: Mutex<Option<Saved>> = Mutex::new(None);

const ALT_ON: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[H";
/// Undo what a remote program may have switched on.
const RESET: &[u8] =
    b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l";
const ALT_OFF: &[u8] = b"\x1b[?1049l";

/// Puts stdin's tty in raw mode; restores on drop, panic or signal exit.
pub struct RawGuard(());

impl RawGuard {
    pub fn enter(altscreen: bool) -> Result<Self, String> {
        let stdin = std::io::stdin();
        let orig = termios::tcgetattr(stdin.as_fd())
            .map_err(|_| "stdin is not a terminal (use a tty)".to_string())?;
        let mut raw = orig.clone();
        raw.make_raw();
        *SAVED.lock().unwrap_or_else(|e| e.into_inner()) = Some(Saved {
            termios: orig,
            altscreen,
        });
        termios::tcsetattr(stdin.as_fd(), OptionalActions::Flush, &raw)
            .map_err(|e| format!("cannot set raw mode: {e}"))?;
        if altscreen {
            write_tty(ALT_ON);
        }
        install_panic_hook();
        Ok(RawGuard(()))
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        restore();
    }
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        prev(info);
    }));
}

/// Idempotent: resets modes, leaves the alternate screen, restores termios.
pub fn restore() {
    let saved = SAVED.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(s) = saved {
        write_tty(RESET);
        if s.altscreen {
            write_tty(ALT_OFF);
        }
        let _ = termios::tcsetattr(std::io::stdin().as_fd(), OptionalActions::Flush, &s.termios);
    }
}

pub fn write_tty(bytes: &[u8]) {
    let mut out = std::io::stdout();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// Current (cols, rows) of the tty, 80x24 if unknown.
pub fn size() -> (u16, u16) {
    match termios::tcgetwinsize(std::io::stdin().as_fd()) {
        Ok(w) if w.ws_col > 0 && w.ws_row > 0 => (w.ws_col, w.ws_row),
        _ => (80, 24),
    }
}
