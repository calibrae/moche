//! Command-line parsing (hand rolled: the surface is small).

use moche_core::predict::PredictMode;

pub const USAGE: &str = "\
usage: moche [options] [user@]host [-- command...]
       moche [options] --direct HOST PORT      (key from $MOSH_KEY or --key-stdin)

options:
  --ssh <cmd>                 ssh program to bootstrap with (default: ssh)
  --ssh-arg <arg>             extra argument for ssh, repeatable
  --server <path>             mosh-server path on the remote (default: mosh-server)
  --server-lang <locale>      locale for the remote mosh-server, must be UTF-8
                              (default: en_US.UTF-8; the local LANG is never sent)
  -p, --server-port-range <lo:hi>
                              UDP port range for mosh-server
  --direct HOST PORT          skip ssh: connect to an already running mosh-server
  --key-stdin                 with --direct: read the session key from stdin
  --no-altscreen              do not use the terminal's alternate screen
  --predict <never|adaptive|always>
                              predictive local echo (also --predict=MODE): show typed
                              characters before the server echoes them (default:
                              adaptive, only when the round trip is slow). Unconfirmed
                              characters are underlined on slow links. `always` can
                              briefly show text typed where nothing echoes (passwords)
  --stats                     on exit, print transport counters to stderr (queued bytes,
                              send/receive errors, gaps, datagrams and bytes)
  -V, --version               print version
  -h, --help                  print this help

Ctrl-^ then . quits. Ctrl-^ Ctrl-^ sends a literal Ctrl-^.

exit codes:
  0  session ended (server shutdown or exit key)
  1  runtime error (terminal, socket, key)
  2  usage error
  3  bootstrap failed (ssh or mosh-server)
  128+N  terminated by signal N (SIGHUP=1, SIGTERM=15)
";

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Bootstrap {
        host: String,
        command: Vec<String>,
    },
    Direct {
        host: String,
        port: u16,
        key_stdin: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Opts {
    pub mode: Mode,
    pub ssh: String,
    pub ssh_args: Vec<String>,
    pub server: String,
    /// Requested server locale; `None` means the default.
    pub server_lang: Option<String>,
    pub port_range: Option<String>,
    pub altscreen: bool,
    /// Predictive local echo mode.
    pub predict: PredictMode,
    /// Print transport counters to stderr on exit.
    pub stats: bool,
    /// Hidden debug option: write host diffs straight to the tty.
    pub passthrough: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Run(Box<Opts>),
    Help,
    Version,
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Parsed, String> {
    let mut it = args.into_iter();
    let mut ssh = "ssh".to_string();
    let mut ssh_args = Vec::new();
    let mut server = "mosh-server".to_string();
    let mut server_lang = None;
    let mut port_range = None;
    let mut altscreen = true;
    let mut predict = PredictMode::default();
    let mut stats = false;
    let mut passthrough = false;
    let mut direct: Option<(String, u16)> = None;
    let mut key_stdin = false;
    let mut host: Option<String> = None;
    let mut command = Vec::new();

    fn val(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
        it.next().ok_or_else(|| format!("{flag} needs a value"))
    }

    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--ssh" => ssh = val(&mut it, "--ssh")?,
            "--ssh-arg" => ssh_args.push(val(&mut it, "--ssh-arg")?),
            "--server" => server = val(&mut it, "--server")?,
            "--server-lang" => server_lang = Some(val(&mut it, "--server-lang")?),
            "-p" | "--server-port-range" => {
                let r = val(&mut it, "--server-port-range")?;
                if !valid_range(&r) {
                    return Err(format!("bad port range {r:?}, expected lo:hi or port"));
                }
                port_range = Some(r);
            }
            "--no-altscreen" => altscreen = false,
            "--predict" => predict = val(&mut it, "--predict")?.parse()?,
            s if s.starts_with("--predict=") => predict = s["--predict=".len()..].parse()?,
            "--stats" => stats = true,
            "--passthrough" => passthrough = true,
            "--key-stdin" => key_stdin = true,
            "--direct" => {
                let h = val(&mut it, "--direct")?;
                let p = val(&mut it, "--direct")?;
                let p = p.parse::<u16>().map_err(|_| format!("bad port {p:?}"))?;
                direct = Some((h, p));
            }
            "--" => {
                command.extend(it.by_ref());
            }
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ => {
                if host.is_some() {
                    return Err(format!("unexpected argument {a:?}"));
                }
                host = Some(a);
            }
        }
    }

    let mode = match (direct, host) {
        (Some((host, port)), None) => {
            if !command.is_empty() {
                return Err("a command makes no sense with --direct".into());
            }
            Mode::Direct {
                host,
                port,
                key_stdin,
            }
        }
        (Some(_), Some(_)) => return Err("--direct takes HOST PORT, not a positional host".into()),
        (None, Some(host)) => {
            if key_stdin {
                return Err("--key-stdin only applies to --direct".into());
            }
            Mode::Bootstrap { host, command }
        }
        (None, None) => return Err("missing host".into()),
    };
    Ok(Parsed::Run(Box::new(Opts {
        mode,
        ssh,
        ssh_args,
        server,
        server_lang,
        port_range,
        altscreen,
        predict,
        stats,
        passthrough,
    })))
}

fn valid_range(r: &str) -> bool {
    let mut parts = r.split(':');
    let ok = |s: Option<&str>| s.is_some_and(|s| s.parse::<u16>().is_ok());
    let first = ok(parts.next());
    match parts.next() {
        None => first,
        Some(b) => first && b.parse::<u16>().is_ok() && parts.next().is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Parsed, String> {
        parse(s.split_whitespace().map(String::from))
    }

    fn opts(s: &str) -> Opts {
        match p(s).unwrap() {
            Parsed::Run(o) => *o,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bootstrap_with_command() {
        let o = opts("--ssh-arg -J --ssh-arg jump -p 60000:60010 u@h -- ls -l");
        assert_eq!(o.ssh_args, ["-J", "jump"]);
        assert_eq!(o.port_range.as_deref(), Some("60000:60010"));
        assert_eq!(
            o.mode,
            Mode::Bootstrap {
                host: "u@h".into(),
                command: vec!["ls".into(), "-l".into()]
            }
        );
        assert!(o.altscreen);
        assert!(!o.stats);
    }

    #[test]
    fn predict_option() {
        assert_eq!(opts("h").predict, PredictMode::Adaptive);
        assert_eq!(opts("--predict never h").predict, PredictMode::Never);
        assert_eq!(opts("--predict=always h").predict, PredictMode::Always);
        assert_eq!(opts("h --predict=adaptive").predict, PredictMode::Adaptive);
        assert!(p("--predict bogus h").is_err());
        assert!(p("--predict=bogus h").is_err());
        assert!(p("h --predict").is_err());
        assert!(USAGE.contains("--predict"));
    }

    #[test]
    fn stats_flag() {
        assert!(opts("--stats u@h").stats);
        assert!(USAGE.contains("--stats"));
    }

    #[test]
    fn direct() {
        let o = opts("--direct 127.0.0.1 60001 --key-stdin --no-altscreen");
        assert_eq!(
            o.mode,
            Mode::Direct {
                host: "127.0.0.1".into(),
                port: 60001,
                key_stdin: true
            }
        );
        assert!(!o.altscreen);
    }

    #[test]
    fn server_lang() {
        assert_eq!(opts("h").server_lang, None);
        assert_eq!(
            opts("--server-lang fr_FR.UTF-8 h").server_lang.as_deref(),
            Some("fr_FR.UTF-8")
        );
        assert!(p("h --server-lang").is_err());
    }

    #[test]
    fn errors() {
        assert!(p("").is_err());
        assert!(p("--bogus h").is_err());
        assert!(p("--direct h notaport").is_err());
        assert!(p("--direct h 1 other").is_err());
        assert!(p("-p 1:2:3 h").is_err());
        assert!(p("h --key-stdin").is_err());
        assert!(p("a b").is_err());
        assert!(p("--ssh").is_err());
    }

    #[test]
    fn help_version() {
        assert_eq!(p("-h").unwrap(), Parsed::Help);
        assert_eq!(p("x --version").unwrap(), Parsed::Version);
    }
}
