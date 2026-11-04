import Foundation
import MocheFFI

/// One mosh session: crypto, transport and screen model. Sans-IO.
///
/// You own the UDP socket, the clock and the timer: feed in datagrams, keys, resizes
/// and `nowMs`, then run a "pump" (see below). There are no timers, threads or
/// callbacks in here.
///
/// **Not `Sendable`: one owner at a time.** Own it from an actor or a serial queue and
/// never touch it from two threads. `nowMs` is a monotonic millisecond clock that you
/// supply, the same clock for every call of a session.
///
/// Pump, after every event (datagram, keys, resize, timer):
/// `pollTransmit` until nil (send each), `takePaint` (feed to the terminal view),
/// `takeEvents`, then re-arm one timer from `nextDeadlineMs` (which also covers the expiry
/// of predictive echo: run the whole pump, `takePaint` included, when the timer fires).
public final class MocheSession {
    private var handle: OpaquePointer?

    /// Creates a session from the 22-character key of `MOSH CONNECT`.
    ///
    /// The library wipes its own decoded copies; wipe your `key` after this returns.
    /// Queues the initial resize message.
    /// - Throws: `.badKey`, `.badSize` (cols/rows outside 1...1000), `.rngFailure`.
    public init(key: Data, cols: UInt16, rows: UInt16, nowMs: UInt64) throws {
        var h: OpaquePointer?
        let rc = withBytes(key) { p, n in moche_session_new(p, n, cols, rows, nowMs, &h) }
        guard rc == MOCHE_OK, let h else { throw MocheError.make(rc == MOCHE_OK ? MOCHE_ERR_ARG : rc) }
        handle = h
    }

    deinit { _ = moche_session_free(handle) }

    // MARK: input

    /// Feeds one received UDP datagram.
    /// - Returns: `true` if authentic and applied (run your pump); `false` if the
    ///   datagram was dropped (failed authentication, wrong direction, too short).
    ///   Never fatal.
    @discardableResult
    public func handleDatagram(_ data: Data, nowMs: UInt64) throws -> Bool {
        let rc = withBytes(data) { p, n in moche_session_handle_datagram(handle, nowMs, p, n) }
        switch rc {
        case MOCHE_OK: return true
        case MOCHE_AUTH_FAILED: return false
        default: throw MocheError.make(rc)
        }
    }

    /// Queues keystrokes or pasted bytes (up to 16 MiB per call; empty is a no-op).
    /// Run your pump afterwards.
    public func sendKeys(_ keys: Data, nowMs: UInt64) throws {
        if keys.isEmpty { return }
        let rc = withBytes(keys) { p, n in moche_session_send_keys(handle, nowMs, p, n) }
        if rc != MOCHE_OK { throw MocheError.make(rc) }
    }

    /// Tells the server the terminal is now `cols` x `rows` (1...1000).
    public func sendResize(cols: UInt16, rows: UInt16, nowMs: UInt64) throws {
        let rc = moche_session_send_resize(handle, nowMs, cols, rows)
        if rc != MOCHE_OK { throw MocheError.make(rc) }
    }

    // MARK: output

    /// The next datagram to send to the server (a UDP payload of at most
    /// ``Moche/maxDatagram`` bytes), or nil when nothing is due. Call until nil.
    public func pollTransmit(nowMs: UInt64) throws -> Data? {
        var b = MocheBytes()
        let rc = moche_session_poll_transmit(handle, nowMs, &b)
        switch rc {
        case MOCHE_OK: return takeData(&b)
        case MOCHE_NONE: return nil
        default: throw MocheError.make(rc)
        }
    }

    /// Absolute time (your clock) at which `pollTransmit` next has work; at or before
    /// now means "now". Nil: nothing scheduled, cancel your timer.
    public func nextDeadlineMs() throws -> UInt64? {
        var t: UInt64 = 0
        let rc = moche_session_next_deadline_ms(handle, &t)
        switch rc {
        case MOCHE_OK: return t
        case MOCHE_NONE: return nil
        default: throw MocheError.make(rc)
        }
    }

    /// Bytes that bring the terminal view from what was last painted to the current
    /// screen, or nil if it is already current. Feed all of them, in order, to an
    /// xterm-like view (SwiftTerm) before the next call. The first paint is full.
    public func takePaint() throws -> Data? {
        var b = MocheBytes()
        let rc = moche_session_take_paint(handle, &b)
        switch rc {
        case MOCHE_OK: return takeData(&b)
        case MOCHE_NONE: return nil
        default: throw MocheError.make(rc)
        }
    }

    /// Makes the next `takePaint` a full repaint. Call after the view was recreated,
    /// cleared or resized.
    public func forceFullPaint() {
        _ = moche_session_force_full_paint(handle)
    }

    // MARK: state

    /// Terminal modes of the latest server state. All false if the session is dead.
    public func modes() -> MocheModes {
        var c = MocheFFI.MocheModes()
        var m = MocheModes()
        guard moche_session_modes(handle, &c) == MOCHE_OK else { return m }
        m.mouseMode = c.mouse_mode
        m.mouseEncoding = c.mouse_encoding
        m.appCursorKeys = c.app_cursor_keys != 0
        m.bracketedPaste = c.bracketed_paste != 0
        m.cursorVisible = c.cursor_visible != 0
        m.altScreen = c.alt_screen != 0
        return m
    }

    /// Pending events; one-shot bits are cleared by the call. Empty if the session is dead.
    public func takeEvents() -> MocheEvents {
        var e: UInt32 = 0
        guard moche_session_take_events(handle, &e) == MOCHE_OK else { return [] }
        return MocheEvents(rawValue: e)
    }

    /// The new window title if it changed since the last call, else nil.
    public func takeTitle() -> String? {
        var b = MocheBytes()
        guard moche_session_take_title(handle, &b) == MOCHE_OK else { return nil }
        return String(decoding: takeData(&b), as: UTF8.self)
    }

    /// Size of the server framebuffer that `takePaint` draws ((0, 0) if the session is dead).
    public var screenSize: (cols: UInt16, rows: UInt16) {
        var c: UInt16 = 0, r: UInt16 = 0
        guard moche_session_screen_size(handle, &c, &r) == MOCHE_OK else { return (0, 0) }
        return (c, r)
    }

    // MARK: backpressure

    /// Input backlog and transport counters. Poll it from your pump to pace a big paste:
    /// feed the next slice only while `queuedBytes` is below a few hundred KB.
    /// - Throws: `.sessionDead` if the session died after an internal error.
    public func stats() throws -> MocheStats {
        var c = MocheFFI.MocheStats()
        let rc = moche_session_stats(handle, &c)
        if rc != MOCHE_OK { throw MocheError.make(rc) }
        var s = MocheStats()
        s.queuedBytes = c.queued_bytes
        s.sendErrors = c.send_errors
        s.rxErrors = c.rx_errors
        s.rxGaps = c.rx_gaps
        return s
    }

    // MARK: predictive echo

    /// Chooses predictive local echo; the default of a new session is `.never`, so an app
    /// that never calls this behaves exactly as before. Changing the mode drops the pending
    /// predictions (the next `takePaint()` removes them from the view).
    ///
    /// Predictions are drawn by `takePaint()`, as ordinary text or, on a slow link,
    /// underlined (SGR 4, now part of the paint stream). They expire on a timer that is
    /// already folded into `nextDeadlineMs()`: when your timer fires, run the whole pump
    /// (`takePaint` included), or an expired guess stays on screen.
    /// - Throws: `.sessionDead` after an internal error.
    public func setPredictMode(_ mode: PredictMode) throws {
        try setPredictMode(rawValue: mode.rawValue)
    }

    /// Raw form, so tests can reach the library's refusal of an unknown mode (`.arg`).
    func setPredictMode(rawValue: Int32) throws {
        let rc = moche_session_set_predict_mode(handle, rawValue)
        if rc != MOCHE_OK { throw MocheError.make(rc) }
    }

    /// Predictive echo counters and state (all zero, mode `.never`, on a session that
    /// never enabled it).
    /// - Throws: `.sessionDead` if the session died after an internal error.
    public func predictStats() throws -> MochePredictStats {
        var c = MocheFFI.MochePredictStats()
        let rc = moche_session_predict_stats(handle, &c)
        if rc != MOCHE_OK { throw MocheError.make(rc) }
        var s = MochePredictStats()
        s.predictions = c.predictions
        s.confirmed = c.confirmed
        s.contradicted = c.contradicted
        s.expired = c.expired
        s.displaying = c.displaying != 0
        s.mode = PredictMode(rawValue: Int32(c.mode)) ?? .never
        return s
    }

    // MARK: link health

    /// Milliseconds since the last new authentic datagram from the server (or since
    /// creation). The server heartbeats about every 3 s; "connection lost" is your UI
    /// decision. 0 if the session is dead.
    public func msSinceLastContact(nowMs: UInt64) -> UInt64 {
        var ms: UInt64 = 0
        return moche_session_ms_since_last_contact(handle, nowMs, &ms) == MOCHE_OK ? ms : 0
    }

    /// Smoothed round-trip time in milliseconds (1000 until the first sample).
    public var srttMs: UInt32 {
        var v: UInt32 = 0
        return moche_session_srtt_ms(handle, &v) == MOCHE_OK ? v : 0
    }
}
