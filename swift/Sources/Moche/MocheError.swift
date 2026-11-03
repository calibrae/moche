import MocheFFI

/// An error from the moche C API. Every case carries the message from
/// `moche_last_error`, captured at the throw site (it never contains key material).
public enum MocheError: Error, Sendable, Equatable {
    /// A pointer argument was NULL (programming error).
    case arg(String)
    /// The key is not 22 characters of valid unpadded base64.
    case badKey(String)
    /// No valid `MOSH CONNECT <port> <key>` line.
    case noConnectLine(String)
    /// An internal bug panicked inside the call; the session is dead.
    case panicked(String)
    /// The session died after an earlier panic: drop it and start over.
    case sessionDead(String)
    /// Input larger than allowed (connect buffer > 1 MiB, keys > 16 MiB per call).
    case tooLarge(String)
    /// cols or rows outside 1...1000.
    case badSize(String)
    /// The operating system random source failed.
    case rngFailure(String)
    /// A negative code this wrapper does not know (newer library).
    case unknown(Int32, String)

    /// The C error code.
    public var code: Int32 {
        switch self {
        case .arg: return MOCHE_ERR_ARG
        case .badKey: return MOCHE_ERR_BAD_KEY
        case .noConnectLine: return MOCHE_ERR_NO_CONNECT
        case .panicked: return MOCHE_ERR_PANIC
        case .sessionDead: return MOCHE_ERR_DEAD
        case .tooLarge: return MOCHE_ERR_TOO_LARGE
        case .badSize: return MOCHE_ERR_BAD_SIZE
        case .rngFailure: return MOCHE_ERR_RNG
        case .unknown(let c, _): return c
        }
    }

    /// The library's description of the failure.
    public var message: String {
        switch self {
        case .arg(let m), .badKey(let m), .noConnectLine(let m), .panicked(let m),
             .sessionDead(let m), .tooLarge(let m), .badSize(let m), .rngFailure(let m),
             .unknown(_, let m):
            return m
        }
    }

    /// Builds the error for a negative `code`, reading this thread's last error now.
    static func make(_ code: Int32) -> MocheError {
        let m = lastErrorMessage()
        switch code {
        case MOCHE_ERR_ARG: return .arg(m)
        case MOCHE_ERR_BAD_KEY: return .badKey(m)
        case MOCHE_ERR_NO_CONNECT: return .noConnectLine(m)
        case MOCHE_ERR_PANIC: return .panicked(m)
        case MOCHE_ERR_DEAD: return .sessionDead(m)
        case MOCHE_ERR_TOO_LARGE: return .tooLarge(m)
        case MOCHE_ERR_BAD_SIZE: return .badSize(m)
        case MOCHE_ERR_RNG: return .rngFailure(m)
        default: return .unknown(code, m)
        }
    }
}

/// This thread's last error message from the library.
func lastErrorMessage() -> String {
    let n = moche_last_error(nil, 0)
    if n == 0 { return "" }
    var buf = [UInt8](repeating: 0, count: n + 1)
    let written = buf.withUnsafeMutableBufferPointer { moche_last_error($0.baseAddress, $0.count) }
    return String(decoding: buf[0..<min(written, n)], as: UTF8.self)
}
