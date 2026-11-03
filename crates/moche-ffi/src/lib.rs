//! `extern "C"` API over moche-core. The only crate allowed to use `unsafe`.
//!
//! The contract is `include/moche.h` (hand-written; keep the two in step,
//! `tools/ffi/check-header.sh` compares the symbol lists). Rules of this file:
//!
//! * every export returns an `i32` code (except the three documented ones) and
//!   runs inside `catch_unwind`; a panic becomes `MOCHE_ERR_PANIC` and kills the
//!   session it happened in (`MOCHE_ERR_DEAD` afterwards);
//! * every `unsafe` block is one tiny pointer operation with a SAFETY comment;
//! * the key is never logged, never put in an error message, and the decoded
//!   copy is wiped when the call that made it returns.

#![deny(unsafe_op_in_unsafe_fn)]
// Exports are plain `extern "C"` fns taking raw pointers: the validity contract
// is documented in moche.h, and C callers cannot see Rust's `unsafe` anyway.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod connect;
mod session;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::ffi::c_char;
use std::mem::ManuallyDrop;
use std::panic::{catch_unwind, AssertUnwindSafe};

use connect::ConnectError;
use session::{dim_ok, Core, Fed, MAX_KEYS};

pub const MOCHE_OK: i32 = 0;
pub const MOCHE_NONE: i32 = 1;
pub const MOCHE_AUTH_FAILED: i32 = 2;
pub const MOCHE_ERR_ARG: i32 = -1;
pub const MOCHE_ERR_BAD_KEY: i32 = -2;
pub const MOCHE_ERR_NO_CONNECT: i32 = -3;
pub const MOCHE_ERR_PANIC: i32 = -4;
pub const MOCHE_ERR_DEAD: i32 = -5;
pub const MOCHE_ERR_TOO_LARGE: i32 = -6;
pub const MOCHE_ERR_BAD_SIZE: i32 = -7;
pub const MOCHE_ERR_RNG: i32 = -8;

/// Opaque session handle.
pub struct MocheSession(Core);

/// Heap buffer handed to the host; free with [`moche_bytes_free`].
#[repr(C)]
#[derive(Debug)]
pub struct MocheBytes {
    pub ptr: *mut u8,
    pub len: usize,
    pub cap: usize,
}

impl MocheBytes {
    const EMPTY: MocheBytes = MocheBytes {
        ptr: std::ptr::null_mut(),
        len: 0,
        cap: 0,
    };

    fn from_vec(v: Vec<u8>) -> MocheBytes {
        if v.is_empty() {
            return Self::EMPTY;
        }
        let mut v = ManuallyDrop::new(v);
        MocheBytes {
            ptr: v.as_mut_ptr(),
            len: v.len(),
            cap: v.capacity(),
        }
    }
}

/// Snapshot of the terminal modes the host's key row cares about.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MocheModes {
    pub mouse_mode: u16,
    pub mouse_encoding: u16,
    pub app_cursor_keys: u8,
    pub bracketed_paste: u8,
    pub cursor_visible: u8,
    pub alt_screen: u8,
}

/// Input backlog and transport counters, see `moche.h`. 40 bytes, 8-aligned,
/// no padding: `queued_bytes` (8) + three `u32` (12) + `reserved[5]` (20).
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct MocheStats {
    pub queued_bytes: u64,
    pub send_errors: u32,
    pub rx_errors: u32,
    pub rx_gaps: u32,
    pub reserved: [u32; 5],
}

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Records `msg` as this thread's last error and returns `code`. Messages are
/// static text or panic payloads of our own code, never key material.
fn fail(code: i32, msg: &str) -> i32 {
    LAST_ERROR.with(|e| {
        let mut e = e.borrow_mut();
        e.clear();
        e.push_str(msg);
    });
    code
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    let m = p
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| p.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown");
    format!("internal panic: {m}")
}

/// Runs `f`, turning a panic into `MOCHE_ERR_PANIC`.
fn guard(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(c) => c,
        Err(p) => fail(MOCHE_ERR_PANIC, &panic_text(&*p)),
    }
}

/// Runs `f` on the session behind `s`. A panic inside poisons the session.
fn with_session(s: *mut MocheSession, f: impl FnOnce(&mut Core) -> i32) -> i32 {
    guard(|| {
        // SAFETY: the host guarantees `s` is null or a live handle it owns
        // exclusively for the duration of the call (documented in moche.h).
        let Some(MocheSession(core)) = (unsafe { s.as_mut() }) else {
            return fail(MOCHE_ERR_ARG, "null session");
        };
        if core.dead.is_some() {
            return fail(MOCHE_ERR_DEAD, "session is dead after an internal panic");
        }
        match catch_unwind(AssertUnwindSafe(|| f(&mut *core))) {
            Ok(c) => c,
            Err(p) => {
                core.dead = Some(MOCHE_ERR_PANIC);
                fail(MOCHE_ERR_PANIC, &panic_text(&*p))
            }
        }
    })
}

/// Borrows `(p, len)` as a slice; `(NULL, 0)` is the empty slice.
fn bytes_in<'a>(p: *const u8, len: usize) -> Result<&'a [u8], i32> {
    if len == 0 {
        return Ok(&[]);
    }
    if p.is_null() {
        return Err(fail(MOCHE_ERR_ARG, "null data with non-zero length"));
    }
    if len > isize::MAX as usize {
        return Err(fail(MOCHE_ERR_TOO_LARGE, "length too large"));
    }
    // SAFETY: non-null, and the host promises `len` readable bytes at `p`
    // that stay untouched during the call.
    Ok(unsafe { std::slice::from_raw_parts(p, len) })
}

/// Writes `v` through the out-pointer `p`.
fn put<T>(p: *mut T, v: T) -> Result<(), i32> {
    if p.is_null() {
        return Err(fail(MOCHE_ERR_ARG, "null out pointer"));
    }
    // SAFETY: non-null, and the host promises `p` points at writable storage for a `T`.
    unsafe { p.write(v) };
    Ok(())
}

/// Result plumbing: `Err(code)` already recorded its message.
fn run(f: impl FnOnce() -> Result<i32, i32>) -> i32 {
    f().unwrap_or_else(|c| c)
}

fn put_bytes(out: *mut MocheBytes, v: Vec<u8>) -> Result<(), i32> {
    put(out, MocheBytes::from_vec(v))
}

/// Library version, NUL-terminated static string.
#[no_mangle]
pub extern "C" fn moche_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast()
}

/// Copies this thread's last error message (UTF-8) into `buf`, NUL-terminated
/// and truncated at a character boundary to `cap` bytes including the NUL.
/// Returns the full message length (without NUL), like `snprintf`.
#[no_mangle]
pub extern "C" fn moche_last_error(buf: *mut u8, cap: usize) -> usize {
    catch_unwind(|| {
        LAST_ERROR.with(|e| {
            let e = e.borrow();
            if !buf.is_null() && cap > 0 {
                let mut n = e.len().min(cap - 1);
                while !e.is_char_boundary(n) {
                    n -= 1;
                }
                // SAFETY: the host promises `cap` writable bytes at `buf`; n < cap.
                unsafe {
                    std::ptr::copy_nonoverlapping(e.as_ptr(), buf, n);
                    *buf.add(n) = 0;
                }
            }
            e.len()
        })
    })
    .unwrap_or(0)
}

/// Frees a buffer returned by any `moche_*` call and resets `*b` to empty, so
/// freeing the same variable twice is harmless.
#[no_mangle]
pub extern "C" fn moche_bytes_free(b: *mut MocheBytes) -> i32 {
    guard(|| {
        // SAFETY: the host passes null or a pointer to a MocheBytes it owns.
        let Some(slot) = (unsafe { b.as_mut() }) else {
            return fail(MOCHE_ERR_ARG, "null MocheBytes");
        };
        let MocheBytes { ptr, len, cap } = std::mem::replace(slot, MocheBytes::EMPTY);
        if ptr.is_null() || cap == 0 {
            return MOCHE_OK;
        }
        if len > cap {
            return fail(MOCHE_ERR_ARG, "corrupt MocheBytes (len > cap)");
        }
        // SAFETY: (ptr, len, cap) came from `MocheBytes::from_vec` of a Vec<u8>
        // and `*b` was reset above, so it is released exactly once.
        drop(unsafe { Vec::from_raw_parts(ptr, len, cap) });
        MOCHE_OK
    })
}

fn connect_result(
    r: Result<connect::Connect, ConnectError>,
    port_out: *mut u16,
    key_out: *mut u8,
) -> Result<i32, i32> {
    let c = r.map_err(|e| match e {
        ConnectError::TooLarge => fail(MOCHE_ERR_TOO_LARGE, "input too large"),
        ConnectError::NotFound => fail(MOCHE_ERR_NO_CONNECT, "no valid MOSH CONNECT line"),
        ConnectError::BadKey => fail(MOCHE_ERR_BAD_KEY, "MOSH CONNECT key is not a valid key"),
    })?;
    if port_out.is_null() || key_out.is_null() {
        return Err(fail(MOCHE_ERR_ARG, "null out pointer"));
    }
    put(port_out, c.port)?;
    // SAFETY: non-null, and the host promises 23 writable bytes (key + NUL).
    unsafe {
        std::ptr::copy_nonoverlapping(c.key.as_ptr(), key_out, c.key.len());
        *key_out.add(c.key.len()) = 0;
    }
    Ok(MOCHE_OK)
}

/// Parses one `MOSH CONNECT <port> <key>` line. `key_out` receives 22 key
/// characters and a NUL (23 bytes); the host wipes it after `moche_session_new`.
#[no_mangle]
pub extern "C" fn moche_parse_connect(
    line: *const u8,
    len: usize,
    port_out: *mut u16,
    key_out: *mut u8,
) -> i32 {
    guard(|| {
        run(|| {
            let line = bytes_in(line, len)?;
            if line.len() > connect::MAX_SCAN {
                return Err(fail(MOCHE_ERR_TOO_LARGE, "input too large"));
            }
            connect_result(connect::parse_line(line), port_out, key_out)
        })
    })
}

/// Like [`moche_parse_connect`] but scans a whole multi-line buffer (ssh
/// output with banners and noise) for the first valid connect line.
#[no_mangle]
pub extern "C" fn moche_find_connect(
    buf: *const u8,
    len: usize,
    port_out: *mut u16,
    key_out: *mut u8,
) -> i32 {
    guard(|| {
        run(|| {
            let buf = bytes_in(buf, len)?;
            connect_result(connect::find(buf), port_out, key_out)
        })
    })
}

/// Creates a session from the 22-character key.
#[no_mangle]
pub extern "C" fn moche_session_new(
    key: *const u8,
    key_len: usize,
    cols: u16,
    rows: u16,
    now_ms: u64,
    out: *mut *mut MocheSession,
) -> i32 {
    guard(|| {
        run(|| {
            put(out, std::ptr::null_mut())?;
            let key = bytes_in(key, key_len)?;
            if !dim_ok(cols, rows) {
                return Err(fail(MOCHE_ERR_BAD_SIZE, "cols and rows must be 1..=1000"));
            }
            if !session::rng_works() {
                return Err(fail(MOCHE_ERR_RNG, "operating system random source failed"));
            }
            let key = std::str::from_utf8(key)
                .map_err(|_| fail(MOCHE_ERR_BAD_KEY, "key is not a valid key"))?;
            let core = Core::new(key, cols, rows, now_ms)
                .map_err(|_| fail(MOCHE_ERR_BAD_KEY, "key is not a valid 22-character key"))?;
            put(out, Box::into_raw(Box::new(MocheSession(core))))?;
            Ok(MOCHE_OK)
        })
    })
}

/// Destroys a session (wipes its key material). NULL is a no-op.
#[no_mangle]
pub extern "C" fn moche_session_free(s: *mut MocheSession) -> i32 {
    guard(|| {
        if !s.is_null() {
            // SAFETY: `s` came from `moche_session_new` (Box::into_raw) and the
            // host calls free once; the handle is dead afterwards.
            drop(unsafe { Box::from_raw(s) });
        }
        MOCHE_OK
    })
}

/// Feeds one received UDP datagram.
#[no_mangle]
pub extern "C" fn moche_session_handle_datagram(
    s: *mut MocheSession,
    now_ms: u64,
    data: *const u8,
    len: usize,
) -> i32 {
    run_s(s, |core| {
        let data = bytes_in(data, len)?;
        Ok(match core.datagram(now_ms, data) {
            Fed::Accepted => MOCHE_OK,
            Fed::Dropped => MOCHE_AUTH_FAILED,
        })
    })
}

fn run_s(s: *mut MocheSession, f: impl FnOnce(&mut Core) -> Result<i32, i32>) -> i32 {
    with_session(s, |core| run(|| f(core)))
}

/// Queues keystrokes or pasted bytes for the server.
#[no_mangle]
pub extern "C" fn moche_session_send_keys(
    s: *mut MocheSession,
    now_ms: u64,
    data: *const u8,
    len: usize,
) -> i32 {
    run_s(s, |core| {
        if len > MAX_KEYS {
            return Err(fail(MOCHE_ERR_TOO_LARGE, "too many bytes in one call"));
        }
        core.send_keys(now_ms, bytes_in(data, len)?);
        Ok(MOCHE_OK)
    })
}

/// Tells the server the host terminal changed size.
#[no_mangle]
pub extern "C" fn moche_session_send_resize(
    s: *mut MocheSession,
    now_ms: u64,
    cols: u16,
    rows: u16,
) -> i32 {
    run_s(s, |core| {
        if !dim_ok(cols, rows) {
            return Err(fail(MOCHE_ERR_BAD_SIZE, "cols and rows must be 1..=1000"));
        }
        core.send_resize(now_ms, cols, rows);
        Ok(MOCHE_OK)
    })
}

/// Next datagram to send (OK) or none due (NONE).
#[no_mangle]
pub extern "C" fn moche_session_poll_transmit(
    s: *mut MocheSession,
    now_ms: u64,
    out: *mut MocheBytes,
) -> i32 {
    run_s(s, |core| {
        put_bytes(out, Vec::new())?;
        Ok(match core.poll_transmit(now_ms) {
            Some(d) => {
                put_bytes(out, d)?;
                MOCHE_OK
            }
            None => MOCHE_NONE,
        })
    })
}

/// Absolute `now_ms` at which `moche_session_poll_transmit` next has work, or NONE.
#[no_mangle]
pub extern "C" fn moche_session_next_deadline_ms(s: *mut MocheSession, out: *mut u64) -> i32 {
    run_s(s, |core| {
        Ok(match core.next_deadline() {
            Some(d) => {
                put(out, d)?;
                MOCHE_OK
            }
            None => {
                put(out, 0)?;
                MOCHE_NONE
            }
        })
    })
}

/// Bytes to feed the terminal view (OK), or NONE if it is up to date.
#[no_mangle]
pub extern "C" fn moche_session_take_paint(s: *mut MocheSession, out: *mut MocheBytes) -> i32 {
    run_s(s, |core| {
        put_bytes(out, Vec::new())?;
        Ok(match core.take_paint() {
            Some(p) => {
                put_bytes(out, p)?;
                MOCHE_OK
            }
            None => MOCHE_NONE,
        })
    })
}

/// Makes the next paint a full repaint.
#[no_mangle]
pub extern "C" fn moche_session_force_full_paint(s: *mut MocheSession) -> i32 {
    run_s(s, |core| {
        core.force_full_paint();
        Ok(MOCHE_OK)
    })
}

/// Current terminal modes (of the latest server state).
#[no_mangle]
pub extern "C" fn moche_session_modes(s: *mut MocheSession, out: *mut MocheModes) -> i32 {
    run_s(s, |core| {
        let m = core.modes();
        put(
            out,
            MocheModes {
                mouse_mode: m.mouse_mode,
                mouse_encoding: m.mouse_encoding,
                app_cursor_keys: u8::from(m.app_cursor_keys),
                bracketed_paste: u8::from(m.bracketed_paste),
                cursor_visible: u8::from(m.cursor_visible),
                alt_screen: u8::from(m.alt_screen),
            },
        )?;
        Ok(MOCHE_OK)
    })
}

/// Event bits: one-shot ones are cleared by the call, level ones repeat.
#[no_mangle]
pub extern "C" fn moche_session_take_events(s: *mut MocheSession, out: *mut u32) -> i32 {
    run_s(s, |core| {
        put(out, core.take_events())?;
        Ok(MOCHE_OK)
    })
}

/// The window title (UTF-8, no NUL) if it changed since the last call (OK), else NONE.
#[no_mangle]
pub extern "C" fn moche_session_take_title(s: *mut MocheSession, out: *mut MocheBytes) -> i32 {
    run_s(s, |core| {
        put_bytes(out, Vec::new())?;
        Ok(match core.take_title() {
            Some(t) => {
                put_bytes(out, t)?;
                MOCHE_OK
            }
            None => MOCHE_NONE,
        })
    })
}

/// Size of the server-side framebuffer being painted.
#[no_mangle]
pub extern "C" fn moche_session_screen_size(
    s: *mut MocheSession,
    cols: *mut u16,
    rows: *mut u16,
) -> i32 {
    run_s(s, |core| {
        let (c, r) = core.size();
        put(cols, c)?;
        put(rows, r)?;
        Ok(MOCHE_OK)
    })
}

/// Milliseconds since the last authentic datagram (or since creation).
#[no_mangle]
pub extern "C" fn moche_session_ms_since_last_contact(
    s: *mut MocheSession,
    now_ms: u64,
    out: *mut u64,
) -> i32 {
    run_s(s, |core| {
        put(out, core.since_contact(now_ms))?;
        Ok(MOCHE_OK)
    })
}

/// Smoothed round-trip time in milliseconds (1000 before the first sample).
#[no_mangle]
pub extern "C" fn moche_session_srtt_ms(s: *mut MocheSession, out: *mut u32) -> i32 {
    run_s(s, |core| {
        put(out, core.srtt_ms())?;
        Ok(MOCHE_OK)
    })
}

/// Input backlog (backpressure) and transport counters.
#[no_mangle]
pub extern "C" fn moche_session_stats(s: *mut MocheSession, out: *mut MocheStats) -> i32 {
    run_s(s, |core| {
        let c = core.stats();
        put(
            out,
            MocheStats {
                queued_bytes: c.queued_bytes,
                send_errors: c.send_errors,
                rx_errors: c.rx_errors,
                rx_gaps: c.rx_gaps,
                reserved: [0; 5],
            },
        )?;
        Ok(MOCHE_OK)
    })
}

/// Test-only: panics inside a session call, to prove the conversion.
#[cfg(test)]
pub(crate) extern "C" fn moche_session_test_panic(s: *mut MocheSession) -> i32 {
    with_session(s, |_| panic!("test panic"))
}
