//! Embeds the git commit and target triple for `moche --version`.
//! GIT_COMMIT (env) wins, then `git rev-parse --short HEAD`, else "unknown".

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GIT_COMMIT");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    let commit = std::env::var("GIT_COMMIT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let out = Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()?;
            let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
            (out.status.success() && !s.is_empty()).then_some(s)
        })
        .unwrap_or_else(|| "unknown".into());
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=MOCHE_GIT_COMMIT={commit}");
    println!("cargo:rustc-env=MOCHE_TARGET={target}");
}
