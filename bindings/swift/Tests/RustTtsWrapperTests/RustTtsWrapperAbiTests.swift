// ABI conformance tests for the Swift binding — mirrors bindings/c
// (the C acceptance harness). Requires the Rust dylib built and
// TTS_WRAPPER_LIB_DIR set at build time (see Package.swift).

import XCTest
@testable import RustTtsWrapper

final class RustTtsWrapperAbiTests: XCTestCase {
    private func makeClient() throws -> TtsClient {
        try TtsClient(
            engineId: "openai",
            credentials: ["apiKey": "dummy-key-for-swift-tests"]
        )
    }

    func testEngineEnumerationMatchesCount() throws {
        let count = TtsClient.engineCount()
        XCTAssertGreaterThan(count, 0)

        let engines = try TtsClient.listEngines()
        XCTAssertEqual(engines.count, count)
        for e in engines {
            XCTAssertFalse(e.id.isEmpty)
            XCTAssertFalse(e.name.isEmpty)
        }
        XCTAssertTrue(engines.contains { $0.id == "openai" })
    }

    func testCreateCloseRoundTrip() throws {
        let c = try makeClient()
        c.close()
        c.close() // idempotent
        XCTAssertThrowsError(try c.speak("x"))
    }

    func testCreateFailureSurfacesGlobalError() {
        XCTAssertThrowsError(try TtsClient(engineId: "no-such-engine")) { error in
            XCTAssertTrue("\(error)".contains("no-such-engine"))
        }
    }

    func testManyClientsLiveSimultaneously() throws {
        let clients = try (0..<8).map { _ in try makeClient() }
        for c in clients { _ = try c.getVoices() }
        for c in clients { c.close() }
    }

    func testSettersAcceptTypicalValues() throws {
        let c = try makeClient()
        c.setVoice("alloy")
        c.setVoice("")
        c.setRate(1.5)
        c.setPitch(0.8)
        c.setVolume(0.9)
        c.stop()
        c.pause()
        c.resume()
        c.close()
    }

    func testGetVoicesReturnsArray_EmptyOfflineIsFine() throws {
        let c = try makeClient()
        defer { c.close() }
        let voices = try c.getVoices()
        for v in voices { XCTAssertFalse(v.id.isEmpty) }
    }

    func testDummyKeySynthesisFailsObservably() throws {
        let c = try makeClient()
        defer { c.close() }
        XCTAssertThrowsError(try c.speakSync("hello swift"))
        XCTAssertNotNil(c.getLastError())
        XCTAssertThrowsError(try c.synthToBytes("hello swift"))
    }

    func testCallbackRegistrationDoesNotThrow() throws {
        let c = try makeClient()
        defer { c.close() }

        c.setOnAudio { _ in }
        c.setOnBoundary { word, charOffset, charLen, start, end, estimated in
            _ = (word, charOffset, charLen, start, end, estimated)
        }
        c.setOnMark { name, charOffset, start, end in
            _ = (name, charOffset, start, end)
        }
        c.setOnViseme { id, offsetSec in _ = (id, offsetSec) }
        c.setOnStart {}
        c.setOnEnd {}
        c.setOnError { _ in }

        // Clearing is a silent no-op.
        c.setOnAudio(nil)
        c.setOnBoundary(nil)
        c.setOnMark(nil)
        c.setOnViseme(nil)
        c.setOnStart(nil)
        c.setOnEnd(nil)
        c.setOnError(nil)
    }
}
