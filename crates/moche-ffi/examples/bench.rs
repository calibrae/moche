//! Micro-benchmarks of the hot paths (std::time only) with a counting allocator.
//!
//! Lives in moche-ffi because a counting `GlobalAlloc` needs `unsafe`, which
//! the repo allows only in this crate. Run:
//!
//! ```sh
//! CARGO_TARGET_DIR=target/perf cargo run --release -p moche-ffi --example bench
//! ```
//!
//! Prints ns/op, allocations/op and allocated bytes/op per case.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use moche_core::crypto::Session;
use moche_core::network::{fragment, Cipher, Direction, Inner};
use moche_core::proto::{decode_diff, encode_diff, Element, Instruction};
use moche_core::screen::{repaint, Framebuffer, Screen};
use moche_core::ssp::Transport;

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards every call unchanged to the system allocator and only
// bumps two counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(n as u64, Ordering::Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const KEY: &str = "Ixu2un/jwqLpFzsnDHm5lw";

fn run<F: FnMut()>(name: &str, iters: u32, mut f: F) {
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let (a0, b0) = (
        ALLOCS.load(Ordering::Relaxed),
        BYTES.load(Ordering::Relaxed),
    );
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    let ns = t.elapsed().as_nanos() as f64 / f64::from(iters);
    let a = (ALLOCS.load(Ordering::Relaxed) - a0) as f64 / f64::from(iters);
    let b = (BYTES.load(Ordering::Relaxed) - b0) as f64 / f64::from(iters);
    println!("{name:<52} {ns:>12.0} ns/op {a:>8.1} allocs/op {b:>10.0} B/op");
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 24
    }
}

/// Colourful, mostly compressible text like a `ls --color` or log dump.
fn colourful(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Lcg(seed);
    let mut o = Vec::with_capacity(n + 64);
    while o.len() < n {
        if r.next().is_multiple_of(9) {
            o.extend_from_slice(format!("\x1b[38;5;{}m", r.next() % 256).as_bytes());
        }
        let w = 3 + r.next() % 9;
        for _ in 0..w {
            o.push(b'a' + (r.next() % 26) as u8);
        }
        o.push(if r.next().is_multiple_of(11) {
            b'\n'
        } else {
            b' '
        });
        if o.last() == Some(&b'\n') {
            o.push(b'\r');
        }
    }
    o.truncate(n);
    o
}

fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Lcg(seed);
    (0..n).map(|_| (r.next() & 0xff) as u8).collect()
}

fn mk_transport() -> Transport {
    let mut s = 1u64;
    Transport::new(
        &Session::from_base64(KEY).unwrap(),
        0,
        Box::new(move |b| {
            for x in b {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                *x = (s >> 40) as u8;
            }
        }),
    )
}

/// Pre-sealed server frames, state k -> k+1 each, `n` of them.
fn server_frames(n: u64, payload: &[u8]) -> Vec<Vec<Vec<u8>>> {
    let c = Cipher::new(&Session::from_base64(KEY).unwrap());
    let (mut seq, mut id) = (0u64, 1u64);
    (1..=n)
        .map(|k| {
            let ins = Instruction {
                version: 2,
                old_num: k - 1,
                new_num: k,
                ack_num: 0,
                throwaway_num: k - 1,
                diff: encode_diff(&[Element::Data(payload.to_vec()), Element::EchoAck(k)]),
                chaff: vec![],
            };
            let frags = fragment(id, &ins.encode()).unwrap();
            id += 1;
            frags
                .into_iter()
                .map(|f| {
                    let inner = Inner {
                        ts: 1,
                        ts_reply: Some(1),
                        frag: f,
                    };
                    seq += 1;
                    c.seal(Direction::ToClient, seq, &inner.encode()).unwrap()
                })
                .collect()
        })
        .collect()
}

fn main() {
    let session = Session::from_base64(KEY).unwrap();
    let cipher = Cipher::new(&session);

    println!("== crypto");
    for len in [100usize, 1252] {
        let pt = noise(len - 24, 1);
        let sealed = cipher.seal(Direction::ToServer, 5, &pt).unwrap();
        run(&format!("seal {len} B datagram"), 200_000, || {
            black_box(cipher.seal(Direction::ToServer, 5, black_box(&pt)).unwrap());
        });
        run(&format!("open {len} B datagram"), 200_000, || {
            black_box(cipher.open(black_box(&sealed)).unwrap());
        });
    }

    println!("== zlib / fragment (fragment() = compress + cut)");
    let key_ins = Instruction {
        version: 2,
        old_num: 41,
        new_num: 42,
        ack_num: 77,
        throwaway_num: 41,
        diff: encode_diff(&[Element::Data(b"a".to_vec())]),
        chaff: vec![7; 9],
    }
    .encode();
    run(
        &format!("compress keystroke instruction ({} B)", key_ins.len()),
        50_000,
        || {
            black_box(fragment(1, black_box(&key_ins)).unwrap());
        },
    );
    for kib in [16usize, 64] {
        let msg = colourful(kib * 1024, 3);
        run(&format!("compress {kib} KiB colourful text"), 300, || {
            black_box(fragment(1, black_box(&msg)).unwrap());
        });
        let msg = noise(kib * 1024, 4);
        run(&format!("compress {kib} KiB incompressible"), 300, || {
            black_box(fragment(1, black_box(&msg)).unwrap());
        });
    }

    println!("== proto");
    let ins = Instruction {
        version: 2,
        old_num: 41,
        new_num: 42,
        ack_num: 77,
        throwaway_num: 41,
        diff: encode_diff(&[Element::Data(noise(100, 5))]),
        chaff: vec![7; 9],
    };
    run("Instruction::encode (100 B diff)", 500_000, || {
        black_box(black_box(&ins).encode());
    });
    let enc = ins.encode();
    run("Instruction::decode (100 B diff)", 500_000, || {
        black_box(Instruction::decode(black_box(&enc)).unwrap());
    });
    let els = [Element::Data(noise(100, 5))];
    run("encode_diff (100 B)", 500_000, || {
        black_box(encode_diff(black_box(&els)));
    });
    let d = encode_diff(&els);
    run("decode_diff (100 B)", 500_000, || {
        black_box(decode_diff(black_box(&d)).unwrap());
    });

    println!("== Transport");
    {
        let n = 20_000u64;
        let frames = server_frames(n, &colourful(60, 9));
        let mut t = mk_transport();
        let mut i = 0usize;
        run(
            "handle_datagram, small server frame (60 B text)",
            18_000,
            || {
                for d in &frames[i % frames.len()] {
                    black_box(t.handle_datagram(1000, d));
                }
                i += 1;
            },
        );
    }
    {
        let big = noise(85_000, 11);
        let frames = server_frames(220, &big);
        let nf = frames[0].len();
        let mut t = mk_transport();
        let mut i = 0usize;
        run(
            &format!("handle_datagram, burst of {nf} fragments (whole msg)"),
            200,
            || {
                for d in &frames[i] {
                    black_box(t.handle_datagram(1000, d));
                }
                i += 1;
            },
        );
    }
    {
        // client keystroke: send_keys + poll_transmit (+ ack), steady clock
        let mut t = mk_transport();
        let mut now = 1000u64;
        // drain the initial resize-less state
        run("keystroke: send_keys + poll_transmit", 20_000, || {
            t.send_keys(now, b"a");
            now += 10;
            while let Some(d) = t.poll_transmit(now) {
                black_box(d);
            }
            now += 10;
        });
    }
    {
        // heartbeat / ack-only frame
        let mut t = mk_transport();
        let mut now = 0u64;
        run("heartbeat frame (poll_transmit, no data)", 20_000, || {
            now += 3001;
            while let Some(d) = t.poll_transmit(now) {
                black_box(d);
            }
        });
    }
    {
        // resend of a 16 KiB unacked paste state
        let mut t = mk_transport();
        let paste = noise(16 * 1024, 21);
        t.send_keys(0, &paste);
        let mut now = 100u64;
        while let Some(d) = t.poll_transmit(now) {
            black_box(d);
        }
        run(
            "resend of 16 KiB unacked state (poll_transmit)",
            300,
            || {
                now += 2000;
                while let Some(d) = t.poll_transmit(now) {
                    black_box(d);
                }
            },
        );
    }

    println!("== Screen / VT");
    for (cols, rows) in [(80u16, 24u16), (200, 60), (400, 120)] {
        let mut base = Framebuffer::new(cols, rows);
        // fill every row with coloured text
        let mut fill = Vec::new();
        for r in 0..rows {
            fill.extend_from_slice(format!("\x1b[{};1H", r + 1).as_bytes());
            fill.extend_from_slice(&colourful(usize::from(cols) - 1, 40 + u64::from(r)));
        }
        base.feed(&fill);
        let tag = format!("{cols}x{rows}");

        let mut screen = Screen::new(cols, rows);
        screen.apply(0, 1, None, &fill).unwrap();
        let mut n = 1u64;
        let one_line = b"\x1b[10;5Hx";
        run(&format!("Screen::apply one-line diff {tag}"), 2_000, || {
            screen.apply(n, n + 1, None, black_box(one_line)).unwrap();
            n += 1;
            screen.discard_below(n);
        });
        let mut prev = screen.latest().clone();
        let mut k = 0u8;
        run(
            &format!("apply + repaint(prev) one-line {tag}"),
            2_000,
            || {
                let tx = [b'\x1b', b'[', b'1', b'0', b';', b'5', b'H', b'a' + k % 26];
                k = k.wrapping_add(1);
                screen.apply(n, n + 1, None, &tx).unwrap();
                n += 1;
                screen.discard_below(n);
                black_box(screen.repaint(Some(&prev)));
                prev = screen.latest().clone();
            },
        );
        run(
            &format!("repaint(prev) unchanged screen {tag}"),
            2_000,
            || {
                black_box(repaint(screen.latest(), Some(&prev)));
            },
        );
        run(&format!("repaint(None) full {tag}"), 300, || {
            black_box(repaint(screen.latest(), None));
        });
        let fb = screen.latest().clone();
        run(&format!("Framebuffer::clone {tag}"), 2_000, || {
            black_box(black_box(&fb).clone());
        });
        run(
            &format!("Framebuffer::display_eq equal {tag}"),
            2_000,
            || {
                black_box(fb.display_eq(black_box(&prev)));
            },
        );
        let mut full = b"\x1b[2J\x1b[H".to_vec();
        full.extend_from_slice(&fill);
        let mut s2 = Screen::new(cols, rows);
        let mut m = 0u64;
        run(&format!("Screen::apply full redraw {tag}"), 100, || {
            s2.apply(m, m + 1, None, black_box(&full)).unwrap();
            m += 1;
            s2.discard_below(m);
        });
    }
    {
        let text = colourful(100_000, 77);
        let mut fb = Framebuffer::new(80, 24);
        run("VT feed 100 KB colourful text (80x24)", 100, || {
            fb.feed(black_box(&text));
        });
        let mut fb = Framebuffer::new(200, 60);
        run("VT feed 100 KB colourful text (200x60)", 100, || {
            fb.feed(black_box(&text));
        });
        let plain: Vec<u8> = text.iter().copied().filter(|&b| b != 0x1b).collect();
        let mut fb = Framebuffer::new(80, 24);
        run("VT feed 100 KB plain ASCII (80x24)", 100, || {
            fb.feed(black_box(&plain));
        });
    }
}
