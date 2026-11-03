//! moche: a mosh-compatible client. See `moche --help`.

mod args;
mod bootstrap;
mod display;
mod session;
mod status;
mod term;

use std::io::BufRead;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use args::{Mode, Opts, Parsed};
use moche_core::crypto::Session;
use session::{Outcome, Signals};

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
            println!("moche {}", env!("CARGO_PKG_VERSION"));
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

    let signals = Signals {
        term: Arc::new(AtomicBool::new(false)),
        hup: Arc::new(AtomicBool::new(false)),
    };
    for (sig, flag) in [
        (signal_hook::consts::SIGTERM, &signals.term),
        (signal_hook::consts::SIGHUP, &signals.hup),
    ] {
        if let Err(e) = signal_hook::flag::register(sig, Arc::clone(flag)) {
            return fail(EXIT_ERROR, &format!("cannot install signal handler: {e}"));
        }
    }

    let guard = match term::RawGuard::enter(opts.altscreen) {
        Ok(g) => g,
        Err(e) => return fail(EXIT_ERROR, &e),
    };
    let (cols, rows) = term::size();
    let mut display = display::make_display(opts.passthrough, cols, rows);
    let result = session::run(&session, addr, display.as_mut(), &signals);
    drop(guard);

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
