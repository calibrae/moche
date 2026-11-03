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
    private var paste = Data()                     // not yet handed to the session
    private let pasteSlice = 64 * 1024
    private let maxQueued: UInt64 = 256 * 1024     // backpressure threshold
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
    func pasted(_ bytes: Data)      { paste.append(bytes); pump() }   // big pastes go through the pump
    func resized(cols: UInt16, rows: UInt16) { try? session.sendResize(cols: cols, rows: rows, nowMs: now()); pump() }

    private func pump() {
        let n = now()
        // Paste pump: feed slices only while the server keeps up, so a Ctrl-C typed
        // later does not wait behind megabytes of paste.
        while !paste.isEmpty, let st = try? session.stats(), st.queuedBytes < maxQueued {
            let slice = paste.prefix(pasteSlice)
            paste.removeFirst(slice.count)
            try? session.sendKeys(Data(slice), nowMs: n)
        }
        while let d = try? session.pollTransmit(nowMs: n) { send(d) }
        if let p = try? session.takePaint() { paint(p) }
        let ev = session.takeEvents()               // .bell, .title, .shutdown ...
        if ev.contains(.shutdown) { timer?.cancel(); return }
        timer?.cancel()
        // While a paste is paused, also wake up soon: acks re-run the pump too.
        let resume: UInt64? = paste.isEmpty ? nil : n + 50
        if let dl = [try? session.nextDeadlineMs(), resume].compactMap({ $0 }).min() {
            let wait = dl > n ? dl - n : 0
            timer = Task { try? await Task.sleep(for: .milliseconds(wait)); self.pump() }
        }
    }
}
```

## Backpressure for pastes

Everything you send is one ordered stream. A multi-megabyte paste handed over in one
`sendKeys` call means a Ctrl-C typed right after it waits until the whole paste is
acknowledged (over a lossy link that can take long). So pace pastes: keep your own buffer
and, in the pump, feed the next slice (64 KiB or so) only while `try session.stats().queuedBytes`
(input the server has not acked yet) is below a few hundred KB (the sketch uses 256 KiB).
Keystrokes typed meanwhile still go straight to `sendKeys` and wait behind at most that
backlog. `stats()` also reports `sendErrors` (should stay 0), `rxErrors` (server messages
dropped as malformed) and `rxGaps` (updates skipped for a missing base state; benign).

"Connection lost" is a UI decision: `session.msSinceLastContact(nowMs:) > 5000` (the server
heartbeats about every 3 s). Roaming needs nothing: keep sending from the same or a new
socket. The paint stream is for an xterm-like view; see the PAINT STREAM section of `moche.h`.
(The sketch above elides error handling; the `now()` clock must start at or after 0 and never
needs to match any wall clock.)
