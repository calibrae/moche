import Foundation
import XCTest
@testable import Moche

/// A worthless throwaway key used only in tests.
let testKey = "AAECAwQFBgcICQoLDA0ODw"

final class ConnectTests: XCTestCase {
    func testParseConnectLine() throws {
        let r = try Moche.parseConnect(line: "  MOSH CONNECT 60001 \(testKey)\r\n")
        XCTAssertEqual(r.port, 60001)
        XCTAssertEqual(String(decoding: r.key, as: UTF8.self), testKey)
    }

    func testFindConnectInNoisyOutput() throws {
        let out = Data("Welcome to host\r\nMOSH CONNECT 61234 \(testKey)\r\n[detached]\r\n".utf8)
        let r = try Moche.findConnect(in: out)
        XCTAssertEqual(r.port, 61234)
        XCTAssertEqual(r.key.count, Moche.keyLength)
    }

    func testNoConnectLine() {
        XCTAssertThrowsError(try Moche.findConnect(in: Data("nothing here".utf8))) { e in
            guard case .noConnectLine(let m) = e as? MocheError else { return XCTFail("\(e)") }
            XCTAssertFalse(m.isEmpty)
        }
    }

    func testBadKeyInConnectLine() {
        // 21 valid characters then '!': not base64.
        let line = "MOSH CONNECT 5 \(testKey.prefix(21))!\n"
        XCTAssertThrowsError(try Moche.parseConnect(line: line)) { e in
            guard case .badKey = e as? MocheError else { return XCTFail("\(e)") }
            XCTAssertFalse((e as? MocheError)?.message.contains(String(testKey.prefix(8))) ?? true)
        }
    }

    func testVersionAndMaxDatagram() {
        XCTAssertFalse(Moche.version.isEmpty)
        XCTAssertEqual(Moche.maxDatagram, 1252)
    }

    func testErrorCodes() {
        XCTAssertEqual(MocheError.make(-2).code, -2)
        XCTAssertEqual(MocheError.make(-99).code, -99)
        if case .unknown(let c, _) = MocheError.make(-99) { XCTAssertEqual(c, -99) } else { XCTFail() }
    }
}
