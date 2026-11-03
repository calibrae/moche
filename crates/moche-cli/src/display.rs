//! How host state reaches the user's terminal.
//!
//! [`Display`] is the seam between the session loop and the screen model.
//! [`ScreenDisplay`] (the default) keeps numbered framebuffers in
//! `moche_core::screen::Screen` and paints minimal diffs. [`Passthrough`]
//! (hidden `--passthrough`) writes host diffs straight out, for debugging.

use moche_core::screen::{Framebuffer, Screen, ScreenEvent};

/// Consumes events from the transport, produces bytes for the tty.
pub trait Display {
    /// A server state `old` -> `new` (optionally with a size change).
    fn on_host_diff(&mut self, old: u64, new: u64, resize: Option<(u16, u16)>, bytes: &[u8]);
    /// The server will never again diff against a state below `n`.
    fn on_throwaway(&mut self, n: u64);
    /// The local terminal changed size. The server answers with a resize and
    /// a repaint; until then we keep painting the old-size framebuffer
    /// (a smaller terminal clips or wraps it, whatever the terminal does).
    fn on_local_resize(&mut self);
    /// True while a diff we could not apply may have left us out of sync.
    fn out_of_sync(&self) -> bool;
    /// Bytes to write to the tty now (drains pending output).
    fn flush(&mut self) -> Vec<u8>;
    /// Full repaint of the current screen; empty if unsupported.
    fn repaint(&mut self) -> Vec<u8>;
}

/// Writes host bytes through unchanged.
#[derive(Default)]
pub struct Passthrough {
    out: Vec<u8>,
}

impl Display for Passthrough {
    fn on_host_diff(&mut self, _old: u64, _new: u64, _resize: Option<(u16, u16)>, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }
    fn on_throwaway(&mut self, _n: u64) {}
    fn on_local_resize(&mut self) {}
    fn out_of_sync(&self) -> bool {
        false
    }
    fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
    fn repaint(&mut self) -> Vec<u8> {
        Vec::new()
    }
}

/// Framebuffer-backed display.
pub struct ScreenDisplay {
    screen: Screen,
    /// What the terminal currently shows; `None` forces a full repaint.
    shown: Option<Framebuffer>,
    dirty: bool,
    bell: bool,
    desync: bool,
}

impl ScreenDisplay {
    pub fn new(cols: u16, rows: u16) -> Self {
        ScreenDisplay {
            screen: Screen::new(cols, rows),
            shown: None,
            dirty: true,
            bell: false,
            desync: false,
        }
    }
}

impl Display for ScreenDisplay {
    fn on_host_diff(&mut self, old: u64, new: u64, resize: Option<(u16, u16)>, bytes: &[u8]) {
        match self.screen.apply(old, new, resize, bytes) {
            Ok(()) => {
                self.desync = false;
                self.dirty = true;
            }
            // v1: ignore and flag it. The transport still acks this state,
            // so the server may keep diffing against a base we lack; see
            // docs/protocol.md U13 for what a proper recovery would be.
            Err(_) => self.desync = true,
        }
        for ev in self.screen.take_events() {
            // Titles are painted by repaint.
            if ev == ScreenEvent::Bell {
                self.bell = true;
            }
        }
    }
    fn on_throwaway(&mut self, n: u64) {
        self.screen.discard_below(n);
    }
    fn on_local_resize(&mut self) {
        self.shown = None;
        self.dirty = true;
    }
    fn out_of_sync(&self) -> bool {
        self.desync
    }
    fn flush(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if std::mem::take(&mut self.dirty) {
            out = self.screen.repaint(self.shown.as_ref());
            self.shown = Some(self.screen.latest().clone());
        }
        if std::mem::take(&mut self.bell) {
            out.push(0x07);
        }
        out
    }
    fn repaint(&mut self) -> Vec<u8> {
        self.shown = None;
        self.dirty = true;
        self.flush()
    }
}

/// Picks the display implementation.
pub fn make_display(passthrough: bool, cols: u16, rows: u16) -> Box<dyn Display> {
    if passthrough {
        Box::new(Passthrough::default())
    } else {
        Box::new(ScreenDisplay::new(cols, rows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_orders_and_drains() {
        let mut d = Passthrough::default();
        d.on_host_diff(0, 1, None, b"ab");
        d.on_host_diff(1, 2, Some((80, 24)), b"cd");
        assert_eq!(d.flush(), b"abcd");
        assert!(d.flush().is_empty());
    }

    #[test]
    fn screen_display_paints_and_flags_desync() {
        let mut d = ScreenDisplay::new(10, 3);
        d.on_host_diff(0, 1, None, b"hi\x07");
        let out = d.flush();
        assert!(out.windows(2).any(|w| w == b"hi") && out.contains(&7));
        let mut fb = Framebuffer::new(10, 3);
        fb.feed(&out);
        assert!(fb.text().contains("hi"));
        assert!(d.flush().is_empty());
        d.on_host_diff(9, 10, None, b"x");
        assert!(d.out_of_sync());
        d.on_host_diff(1, 2, None, b"!");
        assert!(!d.out_of_sync());
    }
}
