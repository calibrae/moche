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

/// Input backlog and transport counters, from `MocheSession.stats()`.
public struct MocheStats: Sendable, Equatable {
    /// Input bytes (keystrokes, paste) the server has not acknowledged yet. Stop feeding a
    /// big paste while this exceeds a few hundred KB and resume when it drains: whatever is
    /// typed afterwards (a Ctrl-C!) waits behind it in the ordered stream.
    public var queuedBytes: UInt64 = 0
    /// Frames that could not be built. Should stay 0; nonzero means input is stuck.
    public var sendErrors: UInt32 = 0
    /// Server messages dropped as malformed or over a limit.
    public var rxErrors: UInt32 = 0
    /// Server updates not applied because their base state was missing. Benign.
    public var rxGaps: UInt32 = 0

    public init() {}
}

/// When predictive local echo is shown (`MocheSession.setPredictMode(_:)`).
///
/// Display only: the prediction is drawn by `takePaint()` and never changes what is sent.
public enum PredictMode: Int32, Sendable {
    /// Nothing is predicted. The default of every new session.
    case never = 0
    /// Predict while the smoothed round-trip time is above about 30 ms (until it drops
    /// below 20 ms), and only once the server's real echo has confirmed a prediction of
    /// the current epoch. Nothing typed at a prompt that does not echo (a password) is shown.
    case adaptive = 1
    /// Predict at any round-trip time and show the first prediction of an epoch before any
    /// confirmation. **A secret typed where nothing echoes is visible for about one round
    /// trip plus 50 ms** before it is erased: do not offer this to users typing passwords.
    case always = 2
}

/// Predictive echo counters and state, from `MocheSession.predictStats()`.
public struct MochePredictStats: Sendable, Equatable {
    /// Typed keys turned into predictions (shown or hidden), cumulative.
    public var predictions: UInt32 = 0
    /// Predictions the server's echo confirmed, cumulative.
    public var confirmed: UInt32 = 0
    /// Chains cancelled because the server showed something else, cumulative.
    public var contradicted: UInt32 = 0
    /// Chains or frozen overlays dropped by timeout or silence, cumulative.
    public var expired: UInt32 = 0
    /// The overlay currently changes what `takePaint()` draws.
    public var displaying = false
    /// The current mode.
    public var mode: PredictMode = .never

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
