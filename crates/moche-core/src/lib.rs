//! Sans-IO core of moche, a clean-room mosh client.
//!
//! No sockets, threads or clocks live here: the host feeds datagrams,
//! keystrokes, resizes and `now`, and gets back datagrams, deadlines and
//! screen updates. The wire format contract is `docs/protocol.md`.

pub mod crypto;
pub mod network;
pub mod screen;
pub mod ssp;
pub mod vt;
