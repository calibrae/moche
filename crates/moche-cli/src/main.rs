//! moche: a mosh-compatible client. See `moche --help`.

mod args;
mod bootstrap;
mod display;
mod session;
mod status;
mod term;

use std::io::BufRead;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use args::{Mode, Opts, Parsed};
use moche_core::crypto::Session;
use session::{Outcome, Signals, Stats};

const EXIT_ERROR: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_BOOTSTRAP: u8 = 3;

/// First line of `r`, as the session key.
fn read_key_line<R: BufRead>(mut r: R) -> Result<String, String> {
    let mut line = String::new();
    r.read_line(&mut line)
        .map_err(|e| format!("reading key: {e}"))?;
    let k = line.trim();
    if k.is_empty() {
        return Err("no key given".into());
    }
    Ok(k.to_string())
}

/// `moche <version> (<commit>, <target triple>)`.
fn version_string() -> String {
    format!(
        "moche {} ({}, {})",
        env!("CARGO_PKG_VERSION"),
        env!("MOCHE_GIT_COMMIT"),
        env!("MOCHE_TARGET")
    )
}

fn fail(code: u8, msg: &str) -> ExitCode {
    eprintln!("moche: {msg}");
    ExitCode::from(code)
}

fn main() -> ExitCode {
    let opts = match args::parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(o)) => *o,
        Ok(Parsed::Help) => {
            print!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Parsed::Version) => {
            println!("{}", version_string());
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("moche: {e}\n\n{}", args::USAGE);
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let (addr, key) = match connect_info(&opts) {
        Ok(v) => v,
        Err((code, msg)) => return fail(code, &msg),
    };
    let session = match Session::from_base64(&key) {
        Ok(s) => s,
        Err(e) => return fail(EXIT_ERROR, &format!("bad session key: {e}")),
    };
    drop(key);

    let signals = match install_signals() {
        Ok(s) => s,
        Err(e) => return fail(EXIT_ERROR, &format!("cannot install signal handler: {e}")),
    };

    let guard = match term::RawGuard::enter(opts.altscreen) {
        Ok(g) => g,
        Err(e) => return fail(EXIT_ERROR, &e),
    };
    let (cols, rows) = term::size();
    let mut display = display::make_display(opts.passthrough, cols, rows, opts.predict);
    let mut stats = Stats::default();
    let result = session::run(&session, addr, display.as_mut(), &signals, &mut stats);
    drop(guard);
    if opts.stats {
        eprintln!("{}", stats.report());
    }

    match result {
        Ok(Outcome::Shutdown) => {
            eprintln!("[moche: server closed the session]");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Quit) => {
            eprintln!("[moche: exited]");
            ExitCode::SUCCESS
        }
        Ok(Outcome::Signal(n)) => ExitCode::from(128 + n as u8),
        Err(e) => fail(EXIT_ERROR, &e),
    }
}

/// SIGTERM/SIGHUP set a flag; all three of those and SIGWINCH also write to a
/// self-pipe so the event loop's poll wakes immediately.
fn install_signals() -> std::io::Result<Signals> {
    use signal_hook::consts::{SIGHUP, SIGTERM, SIGWINCH};
    let (tx, rx) = UnixStream::pair()?;
    tx.set_nonblocking(true)?;
    rx.set_nonblocking(true)?;
    let signals = Signals {
        term: Arc::new(AtomicBool::new(false)),
        hup: Arc::new(AtomicBool::new(false)),
        wake: rx,
    };
    // Flag first, pipe second: the loop sees the flag when it wakes.
    signal_hook::flag::register(SIGTERM, Arc::clone(&signals.term))?;
    signal_hook::flag::register(SIGHUP, Arc::clone(&signals.hup))?;
    for sig in [SIGTERM, SIGHUP, SIGWINCH] {
        signal_hook::low_level::pipe::register(sig, tx.try_clone()?)?;
    }
    Ok(signals)
}

/// Returns the UDP target and the key; errors carry their exit code.
fn connect_info(opts: &Opts) -> Result<(std::net::SocketAddr, String), (u8, String)> {
    match &opts.mode {
        Mode::Direct {
            host,
            port,
            key_stdin,
        } => {
            let key = if *key_stdin {
                read_key_line(std::io::stdin().lock())
            } else {
                std::env::var("MOSH_KEY").map_err(|_| "set MOSH_KEY or use --key-stdin".into())
            }
            .map_err(|e| (EXIT_ERROR, e))?;
            let addr = bootstrap::resolve_host(host, *port).map_err(|e| (EXIT_ERROR, e))?;
            Ok((addr, key))
        }
        Mode::Bootstrap { host, command } => {
            bootstrap::run(opts, host, command).map_err(|e| (EXIT_BOOTSTRAP, e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_line() {
        assert_eq!(read_key_line(&b"  abc\nrest"[..]).unwrap(), "abc");
        assert!(read_key_line(&b"\n"[..]).is_err());
        assert!(read_key_line(&b""[..]).is_err());
    }
}

#[cfg(test)]
mod version_tests {
    #[test]
    fn version_string_shape() {
        let v = super::version_string();
        let rest = v
            .strip_prefix(concat!("moche ", env!("CARGO_PKG_VERSION"), " ("))
            .expect("prefix");
        let inner = rest.strip_suffix(')').expect("suffix");
        let (commit, target) = inner.split_once(", ").expect("commit, target");
        assert!(!commit.is_empty() && !commit.contains(char::is_whitespace));
        assert!(target.contains('-'), "target triple: {target}");
    }
}
