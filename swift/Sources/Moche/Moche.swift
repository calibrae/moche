import Foundation
import MocheFFI

/// Library-wide helpers: version, limits and the `MOSH CONNECT` bootstrap parsing.
public enum Moche {
    /// Library version string.
    public static var version: String { String(cString: moche_version()) }

    /// Largest UDP payload `MocheSession.pollTransmit` can return. Size receive
    /// buffers to at least this (a stock server sends the same maximum).
    public static var maxDatagram: Int { Int(MOCHE_MAX_DATAGRAM) }

    /// Number of characters of the session key.
    public static var keyLength: Int { Int(MOCHE_KEY_CHARS) }

    /// Finds the first valid `MOSH CONNECT <port> <key>` line in the output of
    /// `mosh-server` (banner and motd around it are ignored; up to 1 MiB).
    ///
    /// `key` is the 22 ASCII characters of the session secret. The caller owns that
    /// `Data`: pass it to `MocheSession.init` and then wipe it
    /// (`key.resetBytes(in: key.indices)`). The temporary C buffer used here is zeroed
    /// before returning.
    public static func findConnect(in output: Data) throws -> (port: UInt16, key: Data) {
        try connect(output, moche_find_connect)
    }

    /// Parses exactly one `MOSH CONNECT <port> <key>` line. Same key rules as
    /// ``findConnect(in:)``.
    public static func parseConnect(line: Data) throws -> (port: UInt16, key: Data) {
        try connect(line, moche_parse_connect)
    }

    /// `parseConnect(line:)` for a `String`.
    public static func parseConnect(line: String) throws -> (port: UInt16, key: Data) {
        try parseConnect(line: Data(line.utf8))
    }

    private typealias ConnectFn = (
        UnsafePointer<UInt8>?, Int, UnsafeMutablePointer<UInt16>?, UnsafeMutablePointer<UInt8>?
    ) -> Int32

    private static func connect(_ input: Data, _ fn: ConnectFn) throws -> (port: UInt16, key: Data) {
        var port: UInt16 = 0
        var buf = [UInt8](repeating: 0, count: Int(MOCHE_KEY_BUF))
        defer { wipe(&buf) }
        let rc = withBytes(input) { p, n in fn(p, n, &port, &buf) }
        guard rc == MOCHE_OK else { throw MocheError.make(rc) }
        return (port, Data(buf[0..<Int(MOCHE_KEY_CHARS)]))
    }
}
