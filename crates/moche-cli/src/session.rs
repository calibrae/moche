//! The single-threaded event loop tying tty, UDP socket and transport.

use std::fs::File;
use std::io::Read;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use moche_core::crypto::Session;
use moche_core::ssp::{Event, Rng, Transport};
use rustix::event::{poll, PollFd, PollFlags};
use rustix::io::Errno;

use crate::display::Display;
use crate::status::{clear_status, draw_status, ExitKey, Status, StatusUpdate};
use crate::term;

/// Longest sleep between polls (also the tty size polling period).
const MAX_WAIT_MS: u64 = 100;

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
        let wait = transport
            .next_deadline_ms()
            .map_or(MAX_WAIT_MS, |d| d.saturating_sub(t).min(MAX_WAIT_MS));
        let timeout = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: (wait * 1_000_000) as _,
        };
        let mut fds = [
            PollFd::new(&stdin, PollFlags::IN),
            PollFd::new(&socket, PollFlags::IN),
        ];
        match poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => return Err(format!("poll: {e}")),
        }
        let stdin_ev = fds[0].revents();
        let sock_ev = fds[1].revents();

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
        }

        match status.update(now().saturating_sub(last_contact)) {
            StatusUpdate::None => {}
            StatusUpdate::Show(text) => term::write_tty(&draw_status(&text, cols, rows)),
            StatusUpdate::Clear => {
                let repaint = display.repaint();
                term::write_tty(&clear_status(&repaint, rows));
            }
        }
    }
}
