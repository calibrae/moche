# Moche (Swift package)

A thin Swift wrapper over moche's C API (`crates/moche-ffi/include/moche.h`). Sans-IO:
no sockets, timers, Dispatch or Network.framework in here; the loop is the app's.
iOS 26+, Swift 6 language mode.

## Build the xcframework (on the Apple build host)

```sh
DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer tools/ios/build-xcframework.sh
```

Needs Xcode, rustup (the script adds the two iOS targets and `llvm-tools`). It builds
`moche-ffi` with the `ios` Cargo profile (no LTO, no debug info), strips the leftover
bitcode and DWARF from rustup's precompiled std objects, and FAILS if any `__LLVM` section,
bitcode magic or `__DWARF` segment remains (`tools/ios/check-no-bitcode.sh`). Output in
`target/xcframework/`: `Moche.xcframework`, `Moche.xcframework.zip` and
`Moche.xcframework.zip.sha256`. The unzipped framework is also copied to
`swift/Moche.xcframework`, where `Package.swift` expects it (git-ignored).

## Add it to sigillo

Either a local package (path dependency on this `swift/` directory, after building), or a
vendored copy: commit `Moche.xcframework.zip` somewhere sigillo can reach and use
`.binaryTarget(name: "MocheFFI", url:..., checksum: <contents of the .sha256>)`, or check
the zip into sigillo and compare `shasum -a 256` with the `.sha256` file in CI.
Then `import Moche`. The C module is called `MocheFFI` (it is the binary target), the Swift
module `Moche`.

Tests need an iOS simulator (the xcframework has iOS slices only; plain `swift test` on macOS
cannot link it):

```sh
cd swift && xcodebuild test -scheme Moche -destination 'platform=iOS Simulator,name=<an iPhone>'
```

## Host loop (actor-owned)

`MocheSession` is not `Sendable`: one owner at a time. Wrap it in an actor and re-run one
pump after every event.

```swift
import Moche
import SwiftTerm

actor MoshConnection {
    private let session: MocheSession
    private let send: @Sendable (Data) -> Void     // UDP send (errors ignored: SSP resends)
    private let paint: @Sendable (Data) -> Void    // feed the terminal view
    private var timer: Task<Void, Never>?
    private let clock = ContinuousClock()
    private let t0 = ContinuousClock.now

    private func now() -> UInt64 {                 // monotonic ms, one clock for the session
        let d = t0.duration(to: clock.now).components
        return UInt64(d.seconds) * 1000 + UInt64(d.attoseconds / 1_000_000_000_000_000)
    }

    init(connectOutput: Data, cols: UInt16, rows: UInt16,
         send: @escaping @Sendable (Data) -> Void, paint: @escaping @Sendable (Data) -> Void) throws {
        var (port, key) = try Moche.findConnect(in: connectOutput)   // open UDP to <host>:port
        defer { key.resetBytes(in: key.indices) }                    // wipe our copy
        session = try MocheSession(key: key, cols: cols, rows: rows, nowMs: 0)
        self.send = send; self.paint = paint
        _ = port
    }

    func received(_ datagram: Data) { _ = try? session.handleDatagram(datagram, nowMs: now()); pump() }
    func typed(_ bytes: Data)       { try? session.sendKeys(bytes, nowMs: now()); pump() }
    func resized(cols: UInt16, rows: UInt16) { try? session.sendResize(cols: cols, rows: rows, nowMs: now()); pump() }

    private func pump() {
        let n = now()
        while let d = try? session.pollTransmit(nowMs: n) { send(d) }
        if let p = try? session.takePaint() { paint(p) }
        let ev = session.takeEvents()               // .bell, .title, .shutdown ...
        if ev.contains(.shutdown) { timer?.cancel(); return }
        timer?.cancel()
        if let dl = try? session.nextDeadlineMs() {
            let wait = dl > n ? dl - n : 0
            timer = Task { try? await Task.sleep(for: .milliseconds(wait)); self.pump() }
        }
    }
}
```

"Connection lost" is a UI decision: `session.msSinceLastContact(nowMs:) > 5000` (the server
heartbeats about every 3 s). Roaming needs nothing: keep sending from the same or a new
socket. The paint stream is for an xterm-like view; see the PAINT STREAM section of `moche.h`.
(The sketch above elides error handling; the `now()` clock must start at or after 0 and never
needs to match any wall clock.)
