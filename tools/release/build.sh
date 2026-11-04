#!/bin/bash
# Build the release `moche` CLI for one target.
# Usage: tools/release/build.sh <target>
#   x86_64-unknown-linux-musl, aarch64-unknown-linux-musl  (static, cross-built with cargo-zigbuild + pinned zig)
#   aarch64-apple-darwin                                   (native; needs a macOS arm64 host)
# Output: target/<target>/release/moche
# Env: MOCHE_CODESIGN_IDENTITY  Developer ID identity for the macOS binary (hardened runtime); skipped if unset.
#      SOURCE_DATE_EPOCH        defaults to the HEAD commit date.
#      GIT_COMMIT               defaults to `git rev-parse --short HEAD` (read by build.rs).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/env.sh"
[ $# -eq 1 ] || { echo "usage: $0 <target>" >&2; exit 2; }
T="$1"; check_target "$T"
export PATH="$HOME/.cargo/bin:$PATH"
cd "$ROOT"
export_sde

# Pinned zig (hermetic: CI runners need no brew). Source of URLs and hashes:
# https://ziglang.org/download/index.json (zig 0.17.1, released 2026-11-02). The aarch64-macos hash was
# also recomputed locally from the downloaded tarball. Bump version + all four hashes together.
ZIG_VERSION=0.17.1
zig_pin() {
  case "$(uname -s)-$(uname -m)" in
    Darwin-arm64)   ZIG_ARCH=aarch64-macos; ZIG_SHA=75f1bc7a1e9852f17ec9a0c1895f767c4ec56d46bc8a834920eece1d0359dd8d;;
    Darwin-x86_64)  ZIG_ARCH=x86_64-macos;  ZIG_SHA=9af07b20319fbd0c92ee46ad23e81d6a782b1aaea2cfb8dcbff35f1d2c7ddbb7;;
    Linux-x86_64)   ZIG_ARCH=x86_64-linux;  ZIG_SHA=3581863e3cbcfd0fce0efc682ce7e9e93da96668a977791d356a26d2a7e9b17a;;
    Linux-aarch64)  ZIG_ARCH=aarch64-linux; ZIG_SHA=a1546d5646eb184437918d6635ca6b762a0e889eac09cef720ee7a1ed3fe7795;;
    *) echo "no pinned zig for this host" >&2; exit 1;;
  esac
}

ensure_zig() {
  zig_pin
  local dir="$TOOLS/zig/zig-$ZIG_ARCH-$ZIG_VERSION"
  if [ ! -x "$dir/zig" ]; then
    local tb="zig-$ZIG_ARCH-$ZIG_VERSION.tar.xz" dl="$TOOLS/zig/dl"
    mkdir -p "$dl"
    if [ ! -f "$dl/$tb" ]; then
      curl -fsSL -o "$dl/$tb.part" "https://ziglang.org/download/$ZIG_VERSION/$tb"
      mv "$dl/$tb.part" "$dl/$tb"
    fi
    local got; got=$(shasum -a 256 "$dl/$tb" | cut -d' ' -f1)
    if [ "$got" != "$ZIG_SHA" ]; then
      echo "zig tarball hash mismatch: got $got, want $ZIG_SHA (delete $dl/$tb and retry)" >&2; exit 1
    fi
    tar -xJf "$dl/$tb" -C "$TOOLS/zig"
  fi
  export PATH="$dir:$PATH"
}

ensure_zigbuild() {
  if ! command -v cargo-zigbuild >/dev/null 2>&1; then
    cargo install --locked cargo-zigbuild --root "$TOOLS/cargo"
  fi
  export PATH="$TOOLS/cargo/bin:$PATH"
}

# Path remapping for reproducible-ish output. $HOME-based paths only appear symbolically.
RUSTC_SYSROOT=$(rustc --print sysroot)
REMAP="--remap-path-prefix=$ROOT=/src/moche"
REMAP="$REMAP --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
REMAP="$REMAP --remap-path-prefix=$RUSTC_SYSROOT=/rustc"
REMAP="$REMAP --remap-path-prefix=$HOME=/home/user"
export RUSTFLAGS="${RUSTFLAGS:-} $REMAP -C strip=symbols"
export GIT_COMMIT="${GIT_COMMIT:-$(git rev-parse --short HEAD 2>/dev/null || echo unknown)}"
export CARGO_INCREMENTAL=0

rustup target add "$T" >/dev/null
start=$(date +%s)
case "$T" in
  *-linux-musl)
    ensure_zig; ensure_zigbuild
    cargo zigbuild --release --locked -p moche-cli --target "$T"
    ;;
  aarch64-apple-darwin)
    [ "$(uname -s)-$(uname -m)" = "Darwin-arm64" ] || { echo "$T must be built on a macOS arm64 host" >&2; exit 1; }
    cargo build --release --locked -p moche-cli --target "$T"
    if [ -n "${MOCHE_CODESIGN_IDENTITY:-}" ]; then
      codesign --force --options runtime --timestamp --sign "$MOCHE_CODESIGN_IDENTITY" "$(bin_path "$T")"
      codesign --verify --strict --verbose=1 "$(bin_path "$T")"
    else
      echo "MOCHE_CODESIGN_IDENTITY not set: macOS binary is not Developer-ID signed (linker ad-hoc signature only), not notarized"
    fi
    ;;
esac
echo "built $(bin_path "$T") in $(( $(date +%s) - start ))s ($(wc -c < "$(bin_path "$T")" | tr -d ' ') bytes)"
