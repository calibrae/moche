#!/bin/bash
# Build moche-ffi for iOS (device + simulator) and package Moche.xcframework.
# Meant to run on a macOS host with Xcode (e.g. the iOS build box), not on a dev laptop.
# Usage: tools/ios/build-xcframework.sh [OUT_DIR]   (default: ./target/xcframework)
# Output (OUT_DIR): Moche.xcframework, Moche.xcframework.zip, Moche.xcframework.zip.sha256;
# the xcframework is also copied to swift/ (the Swift package's binaryTarget path).
set -euo pipefail

export DEVELOPER_DIR="${DEVELOPER_DIR:-/Applications/Xcode.app/Contents/Developer}"
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="${1:-$ROOT/target/xcframework}"
HEADERS="$ROOT/crates/moche-ffi/include"   # the real moche.h + module.modulemap
TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim)

cd "$ROOT"
rustup target add "${TARGETS[@]}"
rustup component add llvm-tools   # llvm-objcopy, used by sanitize-archive.sh

# With an explicit --target, RUSTFLAGS applies to the target crates only, not to
# host build scripts. Existing RUSTFLAGS are kept.
export RUSTFLAGS="${RUSTFLAGS:-} -C embed-bitcode=no"

STAGE="$ROOT/target/ios-stage"
rm -rf "$STAGE"; mkdir -p "$STAGE"
libs=()
for t in "${TARGETS[@]}"; do
  cargo build --profile ios --locked -p moche-ffi --target "$t"
  mkdir -p "$STAGE/$t"
  # std's precompiled objects still carry bitcode and DWARF: strip them per object.
  "$HERE/sanitize-archive.sh" "target/$t/ios/libmoche_ffi.a" "$STAGE/$t/libmoche_ffi.a"
  libs+=("$STAGE/$t/libmoche_ffi.a")
done

# Gate: refuse to package anything with bitcode or debug info left.
"$HERE/check-no-bitcode.sh" "${libs[@]}"
"$ROOT/tools/ffi/check-header.sh" "${libs[0]}"

rm -rf "$OUT"
mkdir -p "$OUT"
xcodebuild -create-xcframework \
  -library "${libs[0]}" -headers "$HEADERS" \
  -library "${libs[1]}" -headers "$HEADERS" \
  -output "$OUT/Moche.xcframework"

# Zip for distribution (ditto keeps the layout Xcode and SwiftPM expect) + checksum.
(cd "$OUT" && rm -f Moche.xcframework.zip \
  && ditto -c -k --sequesterRsrc --keepParent Moche.xcframework Moche.xcframework.zip \
  && shasum -a 256 Moche.xcframework.zip > Moche.xcframework.zip.sha256)

# SwiftPM binaryTarget(path:) consumes the unzipped copy next to Package.swift.
rm -rf "$ROOT/swift/Moche.xcframework"
cp -R "$OUT/Moche.xcframework" "$ROOT/swift/Moche.xcframework"

echo "--- result"
ls -l "${libs[@]}"
du -sh "$OUT/Moche.xcframework"
cat "$OUT/Moche.xcframework.zip.sha256"
