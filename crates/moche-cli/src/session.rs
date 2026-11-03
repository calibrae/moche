//! The single-threaded event loop tying tty, UDP socket and transport.

use std::fs::File;
use std::io::Read;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use moche_core::crypto::Session;
use moche_core::ssp::{Event, Rng, Transport};
use rustix::event::{poll, PollFd, PollFlags};
use rustix::io::Errno;

use crate::display::Display;
use crate::status::{
    clear_status, draw_status, next_status_tick_ms, ExitKey, Status, StatusUpdate,
};
use crate::term;

/// Longest sleep between polls when nothing else bounds it.
const MAX_WAIT_MS: u64 = 5000;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Server shut the session down.
    Shutdown,
    /// User pressed the exit key (or stdin closed).
    Quit,
    /// SIGHUP or SIGTERM, with the signal number.
    Signal(i32),
}

pub struct Signals {
    pub term: Arc<AtomicBool>,
    pub hup: Arc<AtomicBool>,
    /// Read end of a non-blocking self-pipe. SIGTERM, SIGHUP and SIGWINCH
    /// each write a byte to it, which wakes `poll`.
    pub wake: UnixStream,
}

impl Signals {
    /// Empties the self-pipe.
    fn drain(&self) {
        let mut b = [0u8; 64];
        while let Ok(n) = rustix::io::read(&self.wake, &mut b) {
            if n == 0 {
                break;
            }
        }
    }
}

/// How long to sleep: until the nearest of the transport deadline and the
/// status tick, never more than `MAX_WAIT_MS`.
fn wait_ms(now: u64, transport_deadline: Option<u64>, status_tick: Option<u64>) -> u64 {
    [transport_deadline, status_tick]
        .into_iter()
        .flatten()
        .map(|d| d.saturating_sub(now))
        .fold(MAX_WAIT_MS, u64::min)
}

fn urandom() -> Result<Rng, String> {
    let mut f = File::open("/dev/urandom").map_err(|e| format!("cannot open /dev/urandom: {e}"))?;
    Ok(Box::new(move |buf: &mut [u8]| {
        if f.read_exact(buf).is_err() {
            // Chaff needs no secrecy: degrade to zeros rather than fail.
            buf.fill(0);
        }
    }))
}

pub fn run(
    session: &Session,
    server: SocketAddr,
    display: &mut dyn Display,
    signals: &Signals,
) -> Result<Outcome, String> {
    let bind: SocketAddr = if server.is_ipv4() {
        "0.0.0.0:0".parse().map_err(|_| "bind addr")?
    } else {
        "[::]:0".parse().map_err(|_| "bind addr")?
    };
    let socket = UdpSocket::bind(bind).map_err(|e| format!("cannot bind UDP socket: {e}"))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| format!("socket: {e}"))?;

    let start = Instant::now();
    let now = || start.elapsed().as_millis() as u64;
    let mut transport = Transport::new(session, now(), urandom()?);
    let (mut cols, mut rows) = term::size();
    transport.send_resize(now(), cols, rows);

    let stdin = std::io::stdin();
    let mut exit_key = ExitKey::default();
    let mut status = Status::default();
    let mut last_contact = now();
    let mut pending_resize: Option<(u16, u16)> = None;
    let mut buf = vec![0u8; 65536];
    let mut shutdown = false;

    loop {
        if signals.term.load(Ordering::Relaxed) {
            return Ok(Outcome::Signal(15));
        }
        if signals.hup.load(Ordering::Relaxed) {
            return Ok(Outcome::Signal(1));
        }

        while let Some(d) = transport.poll_transmit(now()) {
            // Errors (network down mid-roam) are retried by SSP's resends.
            let _ = socket.send_to(&d, server);
        }

        let t = now();
        let wait = wait_ms(
            t,
            transport.next_deadline_ms(),
            next_status_tick_ms(last_contact, t),
        );
        let timeout = rustix::event::Timespec {
            tv_sec: (wait / 1000) as _,
            tv_nsec: ((wait % 1000) * 1_000_000) as _,
        };
        let mut fds = [
            PollFd::new(&stdin, PollFlags::IN),
            PollFd::new(&socket, PollFlags::IN),
            PollFd::new(&signals.wake, PollFlags::IN),
        ];
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(format!("poll: {e}")),
        }
        let stdin_ev = fds[0].revents();
        let sock_ev = fds[1].revents();
        if !fds[2].revents().is_empty() {
            signals.drain();
        }

        if stdin_ev.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
            match rustix::io::read(stdin.as_fd(), &mut buf) {
                Ok(0) => return Ok(Outcome::Quit),
                Ok(n) => {
                    let (keys, quit) = exit_key.feed(&buf[..n]);
                    transport.send_keys(now(), &keys);
                    if quit {
                        return Ok(Outcome::Quit);
                    }
                }
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(e) => return Err(format!("stdin: {e}")),
            }
        }

        if sock_ev.contains(PollFlags::IN) {
            while let Ok((n, _)) = socket.recv_from(&mut buf) {
                let t = now();
                let events = transport.handle_datagram(t, &buf[..n]);
                if !events.is_empty() {
                    last_contact = t;
                }
                for ev in events {
                    match ev {
                        Event::HostResize { width, height, .. } => {
                            pending_resize = Some((width, height));
                        }
                        Event::HostDiff {
                            old_num,
                            new_num,
                            bytes,
                        } => display.on_host_diff(old_num, new_num, pending_resize.take(), &bytes),
                        Event::EchoAck(_) => {}
                        Event::Shutdown => shutdown = true,
                    }
                }
                display.on_throwaway(transport.peer_throwaway());
            }
        }

        let out = display.flush();
        if !out.is_empty() {
            term::write_tty(&out);
        }
        if shutdown {
            return Ok(Outcome::Shutdown);
        }

        let size = term::size();
        if size != (cols, rows) {
            (cols, rows) = size;
            transport.send_resize(now(), cols, rows);
            display.on_local_resize();
        }

        let out = display.flush();
        if !out.is_empty() {
            term::write_tty(&out);
            // The repaint may have covered the status row.
            if let Some(t) = status.shown() {
                term::write_tty(&draw_status(t, cols, rows));
            }
        }

        match status.update(now().saturating_sub(last_contact), display.out_of_sync()) {
            StatusUpdate::None => {}
            StatusUpdate::Show(text) => term::write_tty(&draw_status(&text, cols, rows)),
            StatusUpdate::Clear => {
                let repaint = display.repaint();
                term::write_tty(&clear_status(&repaint, rows));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn wait_is_bounded_by_nearest_deadline() {
        assert_eq!(wait_ms(100, Some(350), None), 250);
        assert_eq!(wait_ms(100, Some(350), Some(200)), 100);
        assert_eq!(wait_ms(100, Some(50), Some(200)), 0);
        assert_eq!(wait_ms(100, None, None), MAX_WAIT_MS);
        assert_eq!(wait_ms(0, Some(60_000), None), MAX_WAIT_MS);
    }

    /// A signal handler's byte on the self-pipe wakes poll at once and is
    /// drained afterwards.
    #[test]
    fn signal_byte_wakes_poll() {
        let (tx, rx) = UnixStream::pair().unwrap();
        rx.set_nonblocking(true).unwrap();
        let signals = Signals {
            term: Arc::new(AtomicBool::new(false)),
            hup: Arc::new(AtomicBool::new(false)),
            wake: rx,
        };
        let long = rustix::event::Timespec {
            tv_sec: 10,
            tv_nsec: 0,
        };
        let mut fds = [PollFd::new(&signals.wake, PollFlags::IN)];
        let t0 = Instant::now();
        (&tx).write_all(&[1]).unwrap();
        assert_eq!(poll(&mut fds, Some(&long)), Ok(1));
        assert!(t0.elapsed().as_secs() < 2);
        signals.drain();
        let none = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let mut fds = [PollFd::new(&signals.wake, PollFlags::IN)];
        assert_eq!(poll(&mut fds, Some(&none)), Ok(0));
    }
}
