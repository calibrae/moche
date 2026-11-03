//! How host state reaches the user's terminal.
//!
//! [`Display`] is the seam between the session loop and the screen model.
//! [`Passthrough`] writes host diffs straight out (correct while diffs chain
//! in order, which they do on a healthy link). [`ScreenDisplay`] will wrap
//! `moche_core::screen::Screen`: see the TODO below.

/// Consumes events from the transport, produces bytes for the tty.
pub trait Display {
    /// A server state `old` -> `new` (optionally with a size change).
    fn on_host_diff(&mut self, old: u64, new: u64, resize: Option<(u16, u16)>, bytes: &[u8]);
    /// The server will never again diff against a state below `n`.
    fn on_throwaway(&mut self, n: u64);
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
    fn flush(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }
    fn repaint(&mut self) -> Vec<u8> {
        Vec::new()
    }
}

// TODO(screen): wire the real screen model. When moche_core::screen lands:
//   1. enable the `screen` feature by default in Cargo.toml (or drop the cfg);
//   2. check the Screen method names below against the final API;
//   3. in `make_display` (bottom of this file) return ScreenDisplay.
#[cfg(feature = "screen")]
pub struct ScreenDisplay {
    screen: moche_core::screen::Screen,
    /// Framebuffer last written to the terminal, for minimal repaints.
    shown: Option<moche_core::screen::Framebuffer>,
    dirty: bool,
}

#[cfg(feature = "screen")]
impl ScreenDisplay {
    pub fn new(cols: u16, rows: u16) -> Self {
        ScreenDisplay {
            screen: moche_core::screen::Screen::new(cols, rows),
            shown: None,
            dirty: false,
        }
    }
}

#[cfg(feature = "screen")]
impl Display for ScreenDisplay {
    fn on_host_diff(&mut self, old: u64, new: u64, resize: Option<(u16, u16)>, bytes: &[u8]) {
        match self.screen.apply(old, new, resize, bytes) {
            Ok(()) => self.dirty = true,
            // Base state missing: skip; the server re-diffs from an acked base.
            Err(_) => {}
        }
    }
    fn on_throwaway(&mut self, n: u64) {
        self.screen.discard_below(n);
    }
    fn flush(&mut self) -> Vec<u8> {
        if !std::mem::take(&mut self.dirty) {
            return Vec::new();
        }
        let bytes = self.screen.repaint(self.shown.as_ref());
        self.shown = Some(self.screen.latest().clone());
        bytes
    }
    fn repaint(&mut self) -> Vec<u8> {
        self.shown = None;
        self.dirty = true;
        self.flush()
    }
}

/// Picks the display implementation.
#[cfg_attr(not(feature = "screen"), allow(unused_variables))]
pub fn make_display(cols: u16, rows: u16) -> Box<dyn Display> {
    #[cfg(feature = "screen")]
    return Box::new(ScreenDisplay::new(cols, rows));
    #[cfg(not(feature = "screen"))]
    Box::new(Passthrough::default())
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
}
