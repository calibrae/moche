# Releasing moche

The CLI ships as three checksummed tarballs: `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl` (fully static) and `aarch64-apple-darwin`.

## Cut a release

1. Bump `version` in `[workspace.package]` of `Cargo.toml`; run `cargo build` so `Cargo.lock` follows.
2. `tools/third-party.sh` to refresh `docs/THIRD_PARTY.md` (shipped in each tarball), then
   `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check licenses`.
3. Commit, then `git tag vX.Y.Z && git push origin vX.Y.Z` (a tag with a suffix, `vX.Y.Z-rc1`, is
   published as a prerelease).
4. `.github/workflows/release.yaml` runs on the tag on a GitHub-hosted `macos-14` runner (Apple
   silicon): it checks that the tag matches `Cargo.toml`, builds the three targets, packages, writes
   `SHA256SUMS`, runs `verify.sh`, then `gh release create` with the three tarballs and `SHA256SUMS`
   and generated notes, using the built-in `GITHUB_TOKEN` (`permissions: contents: write`; no secret to
   set up). Run the workflow by hand (workflow_dispatch, input `tag`) for a dry run: same build and
   verification, nothing published.

## Verify a download

    shasum -a 256 -c SHA256SUMS          # or: sha256sum -c SHA256SUMS
    tar -xzf moche-X.Y.Z-<target>.tar.gz && ./moche-X.Y.Z-<target>/moche --version

`--version` prints `moche X.Y.Z (<commit>, <target>)`.

## Dry-run locally

    tools/release/build.sh <target>      # target/<target>/release/moche
    tools/release/package.sh <target>    # dist/moche-<version>-<target>.tar.gz
    tools/release/checksums.sh dist
    tools/release/verify.sh dist/moche-<version>-<target>.tar.gz <target>

`build.sh` uses the workspace release profile, `--locked`, `--remap-path-prefix`, `-C strip=symbols`
and `SOURCE_DATE_EPOCH` = HEAD commit date. Linux targets are cross-built from macOS or Linux with
`cargo zigbuild` and a pinned zig (version and SHA-256 hard-coded in `build.sh`, downloaded once into
`target/tools/zig`, verified before use; later builds are offline). `cargo-zigbuild` is installed with
`cargo install --locked` into `target/tools/cargo` if not on PATH. Tarballs are deterministic (sorted
names, fixed mtime, numeric owner 0, `gzip -n`): packaging the same binary twice gives the same hash.

Set `MOCHE_CODESIGN_IDENTITY` to a Developer ID identity to sign the macOS binary (hardened runtime);
unset, the binary only carries the linker's ad-hoc signature.

## Reproducibility

Checked 2026-11-04 on a second Mac, fresh clones in different directories, pinned zig:

- Linux tarballs and binaries are bit-for-bit identical across clone paths, hosts and between
  `cargo zigbuild` and the aarch64 linker wrapper, provided the **same rustc** is used (both
  matched hashes built on rustc 1.95.0; the pin is now 1.99.0). A different rustc changes the binary: the Linux hashes
  built with 1.99.0 differ from the 1.95.0 ones. The commit hash and `SOURCE_DATE_EPOCH` are part
  of the input (`GIT_COMMIT` and `SOURCE_DATE_EPOCH` can be exported to rebuild an older identity).
  `rust-toolchain.toml` pins the toolchain (1.99.0, with rustfmt and clippy), so release builds on any
  host use the same rustc. The binary embeds the short commit hash, so the same source under a
  different history (for example a re-exported repository) gives different hashes.
- The macOS binary is reproducible only when built in the **same absolute directory**: ld64 derives
  `LC_UUID` from the linker inputs, including their paths (`--remap-path-prefix` does not reach the
  linker). Two clones at different paths differ in 16 bytes of `LC_UUID` plus the code-signature
  hashes. `-Wl,-no_uuid` removes the difference but the binary then refuses to start
  ("missing LC_UUID load command"), so it is not an option.

## aarch64 Linux link step

rustc 1.99 passes `-Wl,--fix-cortex-a53-843419` when linking `aarch64-unknown-linux-musl`, and zig's
linker rejects it ("unsupported linker arg"), so `cargo zigbuild` fails (rustc 1.95 does not pass it).
`build.sh` therefore builds that target with plain `cargo build` and a small linker wrapper
(`target/tools/linkwrap-<target>.sh`) that drops that flag and hands over to `cargo-zigbuild zig cc`.
Same output as `cargo zigbuild` on a rustc that does not pass the flag (hashes verified identical).
x86_64 still uses `cargo zigbuild` directly.

## Runner requirements

- Release: GitHub-hosted `macos-14` (Apple silicon, rustup, python3, curl, shasum, tar, `gh` are
  preinstalled), network access to download the pinned zig (about 55 MB), `cargo-zigbuild` and the crates.
  A self-hosted runner needs the same, plus a few hundred MB of disk.
- Optional env for macOS signing: `MOCHE_CODESIGN_IDENTITY` (and the identity in the keychain of the
  build host). Not used by the GitHub workflow: its macOS binary carries the linker's ad-hoc signature.

## Bumping zig

Take the new version's URLs and hashes from the official `index.json` published under
`https://ziglang.org/download/`, recompute at least the host hash locally, update `ZIG_VERSION` and the
four hashes in `build.sh` together.

## Known limits

- No notarization; macOS binaries from an unsigned build trigger Gatekeeper on downloaded files
  (`xattr -d com.apple.quarantine`).
- No Windows, no macOS x86_64.
- Linux binaries are built against musl and verified statically on the build host; run-time tests are
  done separately on real machines.
