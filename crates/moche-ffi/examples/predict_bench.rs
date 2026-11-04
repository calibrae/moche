//! Cost of predictive local echo, with a counting allocator (like `bench`).
//!
//! ```sh
//! CARGO_TARGET_DIR=target/perf cargo run --release -p moche-ffi --example predict_bench
//! ```
//!
//! Per typed key: `on_input`, the compose step a host paints (a recycled copy of
//! the server state plus the overlay) and `on_server_state` with the echo.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use moche_core::predict::{PredictMode, Predictor};
use moche_core::screen::Framebuffer;

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
    println!("{name:<58} {ns:>9.0} ns/op {a:>6.2} allocs/op {b:>8.0} B/op");
}

fn screen(cols: u16, rows: u16) -> Framebuffer {
    let mut fb = Framebuffer::new(cols, rows);
    for r in 0..rows - 1 {
        fb.feed(format!("\x1b[{};1Hline of the screen, some text", r + 1).as_bytes());
    }
    fb.feed(format!("\x1b[{};1H$ ", rows).as_bytes());
    fb
}

fn main() {
    for (cols, rows) in [(80u16, 24u16), (200, 60)] {
        println!("== {cols}x{rows}");
        let base = screen(cols, rows);
        let mut echoed = base.clone();
        echoed.feed(b"a");
        let mut scratch = base.clone();

        for (label, mode, srtt) in [
            ("Never", PredictMode::Never, 200.0),
            (
                "Adaptive, fast link (inactive)",
                PredictMode::Adaptive,
                10.0,
            ),
            ("Adaptive, slow link (active)", PredictMode::Adaptive, 200.0),
        ] {
            let mut p = Predictor::new(mode);
            let mut total = 0u64;
            let mut now = 0u64;
            // open the epoch gate once: one echoed key
            p.on_input(now, b"a", &base, srtt, 1);
            p.on_server_state(&echoed, 1, now);
            total += 1;
            run(&format!("{label}: key + compose + echo"), 200_000, || {
                now += 1;
                total += 1;
                p.on_input(now, b"a", black_box(&base), srtt, total);
                if p.is_displaying() {
                    scratch.clone_from(&base);
                    p.apply_overlay(&mut scratch);
                    black_box(&scratch);
                }
                p.on_server_state(black_box(&echoed), total, now);
            });
            println!(
                "    predicted {} confirmed {} mismatches {}",
                p.stats().predicted,
                p.stats().confirmed,
                p.stats().mismatches
            );
        }
        // the inactive costs alone
        let mut p = Predictor::new(PredictMode::Never);
        run(
            "Never: on_server_state only (idle server frame)",
            1_000_000,
            || {
                black_box(p.on_server_state(black_box(&base), 0, 1));
            },
        );
        let mut p = Predictor::new(PredictMode::Adaptive);
        p.tick(0, 10.0, 0);
        run(
            "Adaptive idle: tick + on_server_state, nothing pending",
            1_000_000,
            || {
                black_box(p.tick(5, 10.0, 0));
                black_box(p.on_server_state(black_box(&base), 0, 1));
            },
        );
    }
}
