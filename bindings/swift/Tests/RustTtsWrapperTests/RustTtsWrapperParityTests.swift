// GH #68/#69/#73 parity tests — mirrors the Node/Python/.NET suites.
// Requires the Rust dylib built and TTS_WRAPPER_LIB_DIR set at build
// time (see Package.swift).

import XCTest
@testable import RustTtsWrapper

final class RustTtsWrapperParityTests: XCTestCase {
    private func makeClient(extra: [String: String] = [:]) throws -> TtsClient {
        try TtsClient(
            engineId: "openai",
            credentials: ["apiKey": "dummy-key-for-swift-tests"].merging(extra) { _, new in new }
        )
    }

    // --- GH #68: setup schema ------------------------------------------

    func testEngineSetupsCoverEveryListedEngine() throws {
        let listed = Set(try TtsClient.listEngines().map(\.id))
        let schema = Set(try TtsClient.engineSetups().map(\.id))
        XCTAssertEqual(listed, schema)
    }

    func testAzureRendersKeyAndRegionWithTypedKinds() throws {
        guard let azure = try TtsClient.engineSetup("azure") else {
            return XCTFail("azure compiled in under cloud")
        }
        let key = azure.fields.first { $0.key == "subscriptionKey" }
        XCTAssertEqual(key?.kind, .secret)
        XCTAssertEqual(key?.required, true)
        let region = azure.fields.first { $0.key == "region" }
        XCTAssertEqual(region?.kind, .choice)
        XCTAssertGreaterThan(region?.choices.count ?? 0, 0)
        XCTAssertEqual(azure.capabilities.supportsStreaming, true)
        XCTAssertEqual(azure.capabilities.staticVoiceCatalog, false)
    }

    func testGoogleEitherOrGroupExposed() throws {
        guard let google = try TtsClient.engineSetup("google") else {
            return XCTFail("google compiled in")
        }
        XCTAssertTrue(google.requiredAny.contains { $0.contains("apiKey") && $0.contains("keyFilePath") })
    }

    // --- GH #69: structured errors + tri-state credentials -------------

    func testCreateFailureCarriesEngineNotAvailable() {
        XCTAssertThrowsError(try TtsClient(engineId: "definitely-not-an-engine")) { error in
            guard let ttsError = error as? TtsError else {
                return XCTFail("expected TtsError, got \(error)")
            }
            XCTAssertEqual(ttsError.code, TtsErrorCode.engineNotAvailable.rawValue)
            XCTAssertTrue(ttsError.message.contains("definitely-not-an-engine"))
            XCTAssertNotNil(ttsError.displayMessage)
        }
    }

    func testMissingConfigurationNamesTheField() throws {
        let c = try TtsClient(engineId: "azure", credentials: [:])
        defer { c.close() }
        guard case .checkFailed(let detail) = c.checkCredentials() else {
            return XCTFail("expected checkFailed for empty azure credentials")
        }
        XCTAssertEqual(detail.code, TtsErrorCode.missingConfiguration.rawValue)
        XCTAssertEqual(detail.fieldKey, "subscriptionKey")
        XCTAssertEqual(detail.engineId, "azure")
    }

    func testStaticCatalogIsUnverified() throws {
        // openai has no authenticated voice-list endpoint: the check
        // answers .unverified deterministically, offline.
        let c = try makeClient()
        defer { c.close() }
        guard case .unverified = c.checkCredentials() else {
            return XCTFail("openai must answer .unverified")
        }
    }

    func testSynthesisFailureYieldsStableCodeAndRedaction() throws {
        let c = try makeClient(extra: [
            "apiKey": "sk-abcdef1234567890abcdef",
            "synthUrl": "http://127.0.0.1:1/v1/audio/speech", // closed port
        ])
        defer { c.close() }
        XCTAssertThrowsError(try c.synthToBytes("probe")) { error in
            guard let ttsError = error as? TtsError else {
                return XCTFail("expected TtsError, got \(error)")
            }
            XCTAssertEqual(ttsError.code, TtsErrorCode.networkError.rawValue)
            XCTAssertFalse(ttsError.message.contains("abcdef1234567890"), "secret redacted")
        }
        XCTAssertEqual(c.getLastErrorCode(), .networkError)
        XCTAssertNotNil(c.getLastErrorDetail())
    }

    // --- GH #73: speak_ssml exposed --------------------------------------

    func testSpeakSsmlFailsFastWithStableCode() throws {
        // Raw SSML through the closed-port engine fails with the
        // structured NetworkError (rate/pitch wrapping bypassed).
        let c = try makeClient(extra: [
            "synthUrl": "http://127.0.0.1:1/v1/audio/speech",
        ])
        defer { c.close() }
        XCTAssertThrowsError(
            try c.speakSsml(
                "<speak version=\"1.0\" xml:lang=\"en-US\">"
                    + "<voice name=\"alloy\">Hello</voice></speak>"
            )
        ) { error in
            guard let ttsError = error as? TtsError else {
                return XCTFail("expected TtsError, got \(error)")
            }
            XCTAssertEqual(ttsError.code, TtsErrorCode.networkError.rawValue)
        }
    }
}
