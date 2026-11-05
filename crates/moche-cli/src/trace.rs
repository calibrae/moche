//! Debug trace of the predictive echo, enabled by `MOCHE_PREDICT_TRACE=<file>`.
//!
//! Off (variable unset) it costs one `OnceLock` load per hook and never allocates.
//! On, one text line per event, `<unix seconds.micros> <event> key=value ...`.
//! **The trace contains the typed keys (hex): debugging only, the file is created 0600.**

use std::fmt::Arguments;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static TRACE: OnceLock<Option<Mutex<BufWriter<std::fs::File>>>> = OnceLock::new();

fn sink() -> &'static Option<Mutex<BufWriter<std::fs::File>>> {
    TRACE.get_or_init(|| {
        let path = std::env::var_os("MOCHE_PREDICT_TRACE")?;
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .ok()?;
        Some(Mutex::new(BufWriter::new(f)))
    })
}

/// True if the trace is enabled.
pub fn on() -> bool {
    sink().is_some()
}

/// Seconds since the Unix epoch, with microseconds.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Appends one line (flushed at once: the process may be killed).
pub fn log(args: Arguments) {
    if let Some(m) = sink() {
        if let Ok(mut w) = m.lock() {
            let _ = writeln!(w, "{:.6} {}", now(), args);
            let _ = w.flush();
        }
    }
}

/// Records a tty write: its size and how long the `write` took.
pub fn tty_write(len: usize, t0: f64, t1: f64) {
    log(format_args!(
        "tty_write bytes={len} start={t0:.6} ms={:.3}",
        (t1 - t0) * 1000.0
    ));
}
