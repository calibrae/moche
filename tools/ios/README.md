# tools/ios

Run on the iOS build box (macOS + Xcode + rustup), never on a dev laptop.

- `build-xcframework.sh [OUT]` builds `moche-ffi` for `aarch64-apple-ios` and
  `aarch64-apple-ios-sim` with the `ios` Cargo profile (no LTO) and
  `-C embed-bitcode=no`, runs `sanitize-archive.sh` on each archive, gates the result
  with `check-no-bitcode.sh`, and packages `Moche.xcframework` (+ `.zip` and
  `.zip.sha256`) with the real `crates/moche-ffi/include/moche.h` and
  `module.modulemap`. It also copies the framework to `swift/` (see `swift/README.md`).
- `sanitize-archive.sh IN.a OUT.a` removes `__LLVM` bitcode and DWARF from every member.
  Needed because rustup's precompiled std objects carry both whatever RUSTFLAGS say.
  Needs `llvm-objcopy` (`rustup component add llvm-tools`).
- `check-no-bitcode.sh LIB.a...` fails if a library holds `__LLVM`/`__bitcode`, bitcode
  magic or `__DWARF`. Works on host archives too.
- `measure-footprint.sh [SRC] [WORK]` builds a throwaway probe staticlib that
  reaches `Transport` and `Screen`, links it into a simulator executable with
  `-dead_strip` and prints sizes against an empty `main` (the delta is moche's
  cost in an app). Run the result with `xcrun simctl spawn <device> $WORK/probe`.

Both set `DEVELOPER_DIR` and `PATH` themselves (xcode-select points at CommandLineTools).
