import Foundation
import XCTest
@testable import Moche

/// Throwaway key and one real datagram captured from a stock mosh-server for it
/// (server to client, sequence 7). Same vector as the Rust tests (moche-core network.rs).
private let capturedKey = "Ixu2un/jwqLpFzsnDHm5lw"
private let capturedDatagramHex =
    "80000000000000077d3eff25273194f8472a9e5058d9ea455e55cff4c586b9b6a44ffe88adddf19b5bc49639fa139e5c49a731d46208d75d1e7e65d9661396"

private func data(hex: String) -> Data {
    var d = Data()
    var i = hex.startIndex
    while i < hex.endIndex {
        let j = hex.index(i, offsetBy: 2)
        d.append(UInt8(hex[i..<j], radix: 16)!)
        i = j
    }
    return d
}

final class SessionTests: XCTestCase {
    private func makeSession(key: String = testKey, now: UInt64 = 1000) throws -> MocheSession {
        try MocheSession(key: Data(key.utf8), cols: 80, rows: 24, nowMs: now)
    }

    func testBadKeyThrows() {
        for bad in ["short", String(testKey.prefix(21)) + "!", ""] {
            XCTAssertThrowsError(try MocheSession(key: Data(bad.utf8), cols: 80, rows: 24, nowMs: 0)) { e in
                guard case .badKey = e as? MocheError else { return XCTFail("\(bad): \(e)") }
            }
        }
    }

    func testBadSizeThrows() {
        XCTAssertThrowsError(try MocheSession(key: Data(testKey.utf8), cols: 0, rows: 24, nowMs: 0)) { e in
            guard case .badSize = e as? MocheError else { return XCTFail("\(e)") }
        }
    }

    func testNewSessionYieldsFirstDatagramAtDeadline() throws {
        let s = try makeSession(now: 1000)
        let deadline = try XCTUnwrap(try s.nextDeadlineMs())
        XCTAssertGreaterThanOrEqual(deadline, 1000)
        var sent = 0
        while let d = try s.pollTransmit(nowMs: deadline + 1000) {
            XCTAssertLessThanOrEqual(d.count, Moche.maxDatagram)
            XCTAssertGreaterThan(d.count, 8 + 16)
            sent += 1
        }
        XCTAssertGreaterThan(sent, 0)
    }

    func testSendKeysThenPoll() throws {
        let s = try makeSession(now: 1000)
        try s.sendKeys(Data("ls\r".utf8), nowMs: 1000)
        try s.sendKeys(Data(), nowMs: 1000)  // no-op
        XCTAssertNil(try s.pollTransmit(nowMs: 1000), "collection delay not elapsed")
        let deadline = try XCTUnwrap(try s.nextDeadlineMs())
        let d = try XCTUnwrap(try s.pollTransmit(nowMs: deadline + 500))
        XCTAssertLessThanOrEqual(d.count, Moche.maxDatagram)
        XCTAssertEqual(d[0] & 0x80, 0, "client to server: direction bit clear")
    }

    func testLargeInputStaysWithinMaxDatagram() throws {
        let s = try makeSession(now: 0)
        var g = SystemRandomNumberGenerator()
        let big = Data((0..<100_000).map { _ in UInt8.random(in: 0...255, using: &g) })
        try s.sendKeys(big, nowMs: 0)
        var n = 0
        for now in [500, 1000, 2000] as [UInt64] {
            while let d = try s.pollTransmit(nowMs: now) {
                XCTAssertLessThanOrEqual(d.count, Moche.maxDatagram)
                n += 1
            }
        }
        // Large input is paced (adaptive state size, RTO-paced resends), so only a few
        // dozen datagrams leave in the first two seconds; the point is "many, all small".
        XCTAssertGreaterThan(n, 10)
    }

    func testJunkDatagramsAreDroppedWithoutThrowing() throws {
        let s = try makeSession()
        XCTAssertFalse(try s.handleDatagram(Data(repeating: 7, count: 64), nowMs: 1001))
        XCTAssertFalse(try s.handleDatagram(Data([1, 2, 3]), nowMs: 1002))
        XCTAssertFalse(try s.handleDatagram(Data(), nowMs: 1003))
        XCTAssertFalse(try s.handleDatagram(Data(repeating: 0xff, count: 1500), nowMs: 1004))
    }

    func testStatsFreshIsZeroAndQueueGrowsWithKeys() throws {
        let s = try makeSession()
        XCTAssertEqual(try s.stats(), MocheStats())
        try s.sendKeys(Data(repeating: 0x61, count: 1 << 20), nowMs: 1000)
        let st = try s.stats()
        XCTAssertGreaterThan(st.queuedBytes, 0)
        XCTAssertEqual(st.sendErrors, 0)
        XCTAssertEqual(st.rxErrors, 0)
    }

    func testResize() throws {
        let s = try makeSession()
        try s.sendResize(cols: 120, rows: 40, nowMs: 1000)
        XCTAssertThrowsError(try s.sendResize(cols: 0, rows: 40, nowMs: 1000)) { e in
            guard case .badSize = e as? MocheError else { return XCTFail("\(e)") }
        }
    }

    /// Known answer: a datagram a real mosh-server produced for this key is authentic
    /// and leads to a paint.
    func testCapturedServerDatagramPaints() throws {
        let s = try makeSession(key: capturedKey, now: 0)
        XCTAssertTrue(try s.handleDatagram(data(hex: capturedDatagramHex), nowMs: 5))
        let paint = try XCTUnwrap(try s.takePaint())
        XCTAssertFalse(paint.isEmpty)
        // every paint begins with: hide cursor, reset SGR, reset scroll region, ...
        XCTAssertTrue(paint.starts(with: Data("\u{1b}[?25l\u{1b}[0m".utf8)))
        XCTAssertNil(try s.takePaint(), "view is current")
        XCTAssertLessThan(s.msSinceLastContact(nowMs: 10), 100)
        s.forceFullPaint()
        XCTAssertNotNil(try s.takePaint())
        XCTAssertEqual(s.takeEvents().intersection(.shutdown), [])
        _ = s.modes()
        XCTAssertGreaterThan(s.screenSize.cols, 0)
    }
}
