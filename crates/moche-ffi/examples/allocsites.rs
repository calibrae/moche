//! Which large allocations (>= 4 KiB) does one event cost? Counting allocator that
//! records the size of every big allocation, attributed per operation (a client
//! keystroke frame, an ack frame, a server echo frame with data, an echo-ack-only
//! frame) at 80x24 and 200x60 in steady state. In moche-ffi because a
//! `GlobalAlloc` needs `unsafe`.
//!
//!   CARGO_TARGET_DIR=target/perf cargo run --release -p moche-ffi --example allocsites

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use moche_core::crypto::Session;
use moche_core::network::{fragment, Cipher, Direction, Fragment, Inner};
use moche_core::proto::{encode_diff, Element, Instruction};
use moche_core::screen::{Framebuffer, Screen};
use moche_core::ssp::{Event, Transport};

const BIG: usize = 4096;
const SLOTS: usize = 64;
static LOG: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
static N: AtomicUsize = AtomicUsize::new(0);

struct Counting;
fn note(sz: usize) {
    if sz >= BIG {
        let i = N.fetch_add(1, Ordering::Relaxed);
        if i < SLOTS {
            LOG[i].store(sz, Ordering::Relaxed);
        }
    }
}
// SAFETY: forwards to the system allocator, only records big sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        note(l.size());
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note(n);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static A: Counting = Counting;

fn report(name: &str) {
    let n = N.swap(0, Ordering::Relaxed);
    let sizes: Vec<usize> = (0..n.min(SLOTS))
        .map(|i| LOG[i].load(Ordering::Relaxed))
        .collect();
    println!("  {name:<34} {n} allocs >= 4 KiB {sizes:?}");
}

const KEY: &str = "Ixu2un/jwqLpFzsnDHm5lw";

fn main() {
    let session = Session::from_base64(KEY).unwrap();
    let srv = Cipher::new(&session);
    for (cols, rows) in [(80u16, 24u16), (200, 60)] {
        println!("{cols}x{rows}");
        let mut s = 1u64;
        let mut t = Transport::new(
            &session,
            0,
            Box::new(move |b| {
                for x in b {
                    s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                    *x = (s >> 40) as u8;
                }
            }),
        );
        let mut screen = Screen::new(cols, rows);
        let mut shown: Option<Arc<Framebuffer>> = None;
        let (mut seq, mut fid, mut num, mut now) = (0u64, 1u64, 0u64, 1000u64);
        let frame = |t: &mut Transport,
                     screen: &mut Screen,
                     shown: &mut Option<Arc<Framebuffer>>,
                     data: &[u8],
                     now: u64,
                     seq: &mut u64,
                     fid: &mut u64,
                     num: &mut u64,
                     measure: bool| {
            let mut els = vec![Element::EchoAck(*num)];
            if !data.is_empty() {
                els.insert(0, Element::Data(data.to_vec()));
            }
            let ins = Instruction {
                version: 2,
                old_num: *num,
                new_num: *num + 1,
                ack_num: 0,
                throwaway_num: *num,
                diff: encode_diff(&els),
                chaff: vec![],
            };
            let old = *num;
            *num += 1;
            let frags = fragment(*fid, &ins.encode()).unwrap();
            *fid += 1;
            let dgs: Vec<Vec<u8>> = frags
                .into_iter()
                .map(|f: Fragment| {
                    *seq += 1;
                    srv.seal(
                        Direction::ToClient,
                        *seq,
                        &Inner {
                            ts: 1,
                            ts_reply: Some(1),
                            frag: f,
                        }
                        .encode(),
                    )
                    .unwrap()
                })
                .collect();
            if measure {
                N.store(0, Ordering::Relaxed);
            }
            for d in &dgs {
                for ev in t.handle_datagram(now, d) {
                    if let Event::HostDiff { bytes, .. } = ev {
                        screen.apply(old, old + 1, None, &bytes).unwrap();
                    }
                }
            }
            screen.discard_below(t.peer_throwaway());
            let latest = screen.latest_shared();
            let same = shown
                .as_ref()
                .is_some_and(|p| Arc::ptr_eq(p, &latest) || p.display_eq(&latest));
            let _paint = if same {
                Vec::new()
            } else {
                screen.repaint(shown.as_deref())
            };
            *shown = Some(latest);
        };
        // warm up: pool, buffers, caches
        for i in 0..60 {
            frame(
                &mut t,
                &mut screen,
                &mut shown,
                format!("\x1b[3;{}Hx", 1 + i % 70).as_bytes(),
                now,
                &mut seq,
                &mut fid,
                &mut num,
                false,
            );
            now += 100;
            t.send_keys(now, b"a");
            now += 10;
            while t.poll_transmit(now).is_some() {}
            now += 100;
            while t.poll_transmit(now).is_some() {}
        }
        frame(
            &mut t,
            &mut screen,
            &mut shown,
            b"\x1b[3;5Hy",
            now,
            &mut seq,
            &mut fid,
            &mut num,
            true,
        );
        report("server frame with 1 char (rx path)");
        frame(
            &mut t,
            &mut screen,
            &mut shown,
            b"",
            now + 60,
            &mut seq,
            &mut fid,
            &mut num,
            true,
        );
        report("server echo-ack-only frame");
        now += 200;
        t.send_keys(now, b"a");
        now += 10;
        N.store(0, Ordering::Relaxed);
        while t.poll_transmit(now).is_some() {}
        report("client keystroke frame (tx path)");
        now += 120;
        N.store(0, Ordering::Relaxed);
        while t.poll_transmit(now).is_some() {}
        report("client ack/resend frame (tx path)");
    }
}
