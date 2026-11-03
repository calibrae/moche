//! Starting `mosh-server` over ssh and finding where to send UDP.

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, ToSocketAddrs};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::args::Opts;

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(30);
const LINGER: Duration = Duration::from_secs(3);

/// Finds `MOSH CONNECT <port> <key>` in noisy output (motd, banners...).
pub fn parse_connect(output: &str) -> Option<(u16, String)> {
    output.lines().find_map(|line| {
        let mut w = line.split_whitespace();
        if (w.next()?, w.next()?) != ("MOSH", "CONNECT") {
            return None;
        }
        let port = w.next()?.parse().ok()?;
        let key = w.next()?.to_string();
        w.next().is_none().then_some((port, key))
    })
}

/// The `hostname` value of `ssh -G` output.
pub fn parse_ssh_g(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|l| l.strip_prefix("hostname "))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
}

/// POSIX single-quote escaping for the remote shell.
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,@+%".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The locale for the remote server: `requested` if it is UTF-8, else
/// en_US.UTF-8. The client's own LANG is deliberately not consulted: the
/// server may lack that locale.
pub fn pick_lang(requested: Option<&str>) -> String {
    match requested {
        Some(l)
            if {
                let u = l.to_ascii_lowercase();
                u.contains("utf-8") || u.contains("utf8")
            } =>
        {
            l.to_string()
        }
        _ => "en_US.UTF-8".to_string(),
    }
}

/// The single string handed to ssh as the remote command.
pub fn remote_command(opts: &Opts, lang: &str, command: &[String]) -> String {
    let mut parts = vec![sh_quote(&opts.server), "new".into(), "-s".into()];
    parts.extend(["-c".into(), "256".into()]);
    if let Some(r) = &opts.port_range {
        parts.extend(["-p".into(), sh_quote(r)]);
    }
    parts.extend(["-l".into(), sh_quote(&format!("LANG={lang}"))]);
    if !command.is_empty() {
        parts.push("--".into());
        parts.extend(command.iter().map(|c| sh_quote(c)));
    }
    parts.join(" ")
}

/// Strips `user@` from a destination.
fn bare_host(dest: &str) -> &str {
    dest.rsplit_once('@').map_or(dest, |(_, h)| h)
}

/// Resolves the UDP target for `dest`, asking `ssh -G` for the effective host.
pub fn resolve(opts: &Opts, dest: &str, port: u16) -> Result<SocketAddr, String> {
    let from_ssh = Command::new(&opts.ssh)
        .args(&opts.ssh_args)
        .arg("-G")
        .arg(dest)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_ssh_g(&String::from_utf8_lossy(&o.stdout)));
    let host = from_ssh.unwrap_or_else(|| bare_host(dest).to_string());
    resolve_host(&host, port)
}

pub fn resolve_host(host: &str, port: u16) -> Result<SocketAddr, String> {
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or(addrs.first())
        .copied()
        .ok_or_else(|| format!("no address for {host}"))
}

/// Runs ssh + mosh-server; returns (address, key). Errors are user-facing.
pub fn run(opts: &Opts, dest: &str, command: &[String]) -> Result<(SocketAddr, String), String> {
    let lang = pick_lang(opts.server_lang.as_deref());
    let remote = remote_command(opts, &lang, command);
    let mut child = Command::new(&opts.ssh)
        .args(&opts.ssh_args)
        .arg(dest)
        .arg(&remote)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", opts.ssh))?;

    let stdout = child.stdout.take().ok_or("no ssh stdout")?;
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let start = Instant::now();
    let mut seen = String::new();
    let found = loop {
        let left = BOOTSTRAP_TIMEOUT.saturating_sub(start.elapsed());
        match rx.recv_timeout(left) {
            Ok(line) => {
                seen.push_str(&line);
                seen.push('\n');
                if let Some(c) = parse_connect(&line) {
                    break Some(c);
                }
            }
            Err(_) => break None,
        }
    };

    let Some((port, key)) = found else {
        let _ = child.kill();
        let status = child.wait().ok();
        let mut msg = String::from("bootstrap failed: no MOSH CONNECT line from the server");
        if start.elapsed() >= BOOTSTRAP_TIMEOUT {
            msg.push_str(" (timed out)");
        }
        if let Some(s) = status {
            msg.push_str(&format!(" (ssh: {s})"));
        }
        // Without a CONNECT line the output holds no key.
        let seen = seen.trim();
        if !seen.is_empty() {
            msg.push_str(&format!("\nserver output:\n{seen}"));
        }
        return Err(msg);
    };

    // Let ssh finish on its own, then make sure it is gone.
    let t = Instant::now();
    while t.elapsed() < LINGER {
        if matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();

    let addr = resolve(opts, dest, port)?;
    Ok((addr, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{parse, Parsed};

    #[test]
    fn connect_in_noise() {
        let out = "Welcome to host\n\nMOSH CONNECT 60001 AbCdEfGhIjKlMnOpQrStUv\nmosh-server (mosh 1.4.0)\n[mosh-server detached, pid = 1]\n";
        assert_eq!(
            parse_connect(out),
            Some((60001, "AbCdEfGhIjKlMnOpQrStUv".into()))
        );
        assert_eq!(parse_connect("MOSH CONNECT x key\nnothing"), None);
        assert_eq!(parse_connect("MOSH CONNECT 1 k extra"), None);
        assert_eq!(parse_connect("MOSH CONNECT 70000 k"), None);
        assert_eq!(parse_connect(""), None);
    }

    #[test]
    fn ssh_g() {
        let out = "user alice\nhostname 192.0.2.7\nport 22\n";
        assert_eq!(parse_ssh_g(out).as_deref(), Some("192.0.2.7"));
        assert_eq!(parse_ssh_g("port 22\n"), None);
    }

    #[test]
    fn quoting_and_command() {
        assert_eq!(sh_quote("abc-1"), "abc-1");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(sh_quote(""), "''");
        let Parsed::Run(o) = parse(
            ["-p", "60000:60010", "h", "--", "echo", "a b"]
                .into_iter()
                .map(String::from),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            remote_command(&o, "en_US.UTF-8", &["echo".into(), "a b".into()]),
            "mosh-server new -s -c 256 -p 60000:60010 -l LANG=en_US.UTF-8 -- echo 'a b'"
        );
    }

    #[test]
    fn lang_choice() {
        assert_eq!(pick_lang(None), "en_US.UTF-8");
        assert_eq!(pick_lang(Some("fr_FR.UTF-8")), "fr_FR.UTF-8");
        assert_eq!(pick_lang(Some("de_DE.utf8")), "de_DE.utf8");
        assert_eq!(pick_lang(Some("C")), "en_US.UTF-8");
        assert_eq!(pick_lang(None), "en_US.UTF-8");
    }

    #[test]
    fn host_stripping() {
        assert_eq!(bare_host("u@h"), "h");
        assert_eq!(bare_host("h"), "h");
        assert_eq!(resolve_host("127.0.0.1", 9).unwrap().port(), 9);
    }
}
