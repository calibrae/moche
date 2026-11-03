import MocheFFI

/// Terminal modes of the latest server state, for encoding keys and mouse events.
public struct MocheModes: Sendable, Equatable {
    /// 0 off, else 1000, 1002 or 1003.
    public var mouseMode: UInt16 = 0
    /// 0 default (X10), else 1005, 1006 or 1015.
    public var mouseEncoding: UInt16 = 0
    /// Arrows send `ESC O A` instead of `ESC [ A`.
    public var appCursorKeys = false
    /// Wrap pastes in `ESC[200~` ... `ESC[201~`.
    public var bracketedPaste = false
    /// The server shows the cursor.
    public var cursorVisible = false
    /// A full-screen program is running.
    public var altScreen = false

    public init() {}
}

/// Events reported by `MocheSession.takeEvents()`.
///
/// `bell`, `title` and `resized` are one-shot; `outOfSync` and `shutdown` are
/// levels reported on every call while the condition holds.
public struct MocheEvents: OptionSet, Sendable {
    public let rawValue: UInt32
    public init(rawValue: UInt32) { self.rawValue = rawValue }

    /// The server rang the bell (the paint stream carries no BEL).
    public static let bell = MocheEvents(rawValue: MOCHE_EVENT_BELL)
    /// The title changed: fetch it with `takeTitle()`.
    public static let title = MocheEvents(rawValue: MOCHE_EVENT_TITLE)
    /// The server framebuffer changed size (see `screenSize`).
    public static let resized = MocheEvents(rawValue: MOCHE_EVENT_RESIZED)
    /// An update could not be applied; the screen may be wrong until it heals.
    public static let outOfSync = MocheEvents(rawValue: MOCHE_EVENT_OUT_OF_SYNC)
    /// The server ended the session: paint the last screen, then stop the loop.
    public static let shutdown = MocheEvents(rawValue: MOCHE_EVENT_SHUTDOWN)
}
