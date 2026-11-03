import Foundation
import MocheFFI

/// Runs `body` with a pointer to the bytes of `data` (nil and 0 when empty).
@inline(__always)
func withBytes<R>(_ data: Data, _ body: (UnsafePointer<UInt8>?, Int) throws -> R) rethrows -> R {
    try data.withUnsafeBytes { raw in
        try body(raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
    }
}

/// Copies a library-owned buffer into `Data` and always frees it.
func takeData(_ b: inout MocheBytes) -> Data {
    defer { _ = moche_bytes_free(&b) }
    guard let p = b.ptr, b.len > 0 else { return Data() }
    return Data(bytes: p, count: b.len)
}

/// Overwrites a buffer with zeros in a way the optimizer may not drop.
func wipe(_ buf: inout [UInt8]) {
    buf.withUnsafeMutableBytes { _ = memset_s($0.baseAddress, $0.count, 0, $0.count) }
}
