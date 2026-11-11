import Foundation
// Two supported integrations:
//  * SwiftPM (this package): the CRustTtsWrapper C target wraps the
//    cbindgen header (symlinked from include/tts_wrapper.h).
//  * Xcode: embed TtsClient.swift in a project that also builds the Rust
//    staticlib and imports the header under the crate's module name.
#if canImport(CRustTtsWrapper)
import CRustTtsWrapper
typealias TtsVoiceC = tts_voice
typealias TtsEngineInfoC = tts_engine_info
#elseif canImport(rust_tts_wrapper)
import rust_tts_wrapper
typealias TtsVoiceC = tts_voice
typealias TtsEngineInfoC = tts_engine_info
#endif

/// High-level voice descriptor returned by `TtsClient.getVoices()`.
public struct TtsVoice: Sendable, Hashable {
    public let id: String
    public let name: String
    public let language: String
    public let gender: String
    public let engine: String

    public init(id: String, name: String, language: String, gender: String, engine: String) {
        self.id = id
        self.name = name
        self.language = language
        self.gender = gender
        self.engine = engine
    }
}

/// High-level engine descriptor returned by `TtsClient.listEngines()`.
public struct TtsEngineInfo: Sendable, Hashable {
    public let id: String
    public let name: String
    public let needsCredentials: Bool
    /// Parsed list of credential keys.
    public let credentialKeys: [String]

    public init(id: String, name: String, needsCredentials: Bool, credentialKeys: [String]) {
        self.id = id
        self.name = name
        self.needsCredentials = needsCredentials
        self.credentialKeys = credentialKeys
    }
}

/// Error surfaced by the Rust backend. The message comes from
/// `tts_get_last_error` on the owning context; when the native build
/// provides structured detail (GH #69) the stable `code`, `fieldKey`,
/// `engineId` and `displayMessage` are populated too.
public struct TtsError: Error, CustomStringConvertible {
    public let message: String
    /// Stable classification (`TtsErrorCode.rawValue`, 0 = unknown).
    public let code: UInt32
    public let fieldKey: String
    public let engineId: String
    /// Safe, display-ready hint (nil when unavailable).
    public let displayMessage: String?

    public init(_ message: String, detail: TtsErrorDetail? = nil) {
        self.message = detail?.message.isEmpty == false ? detail!.message : message
        self.code = detail?.code ?? 0
        self.fieldKey = detail?.fieldKey ?? ""
        self.engineId = detail?.engineId ?? ""
        self.displayMessage = detail?.displayMessage.isEmpty == false ? detail!.displayMessage : nil
    }

    public init(_ message: String) {
        self.init(message, detail: nil)
    }

    public var description: String { message }
}

/// Stable error codes (GH #69) — append-only u32 from the C ABI.
public enum TtsErrorCode: UInt32, Sendable {
    case unknown = 0
    case missingConfiguration = 1
    case invalidConfiguration = 2
    case credentialsRejected = 3
    case networkError = 4
    case modelNotFound = 5
    case missingNativeDependency = 6
    case engineNotAvailable = 7
    case languageNotSupported = 8
    case engineFailure = 9

    /// Tolerant mapping: unknown values (newer native builds) degrade
    /// to `.unknown` instead of failing.
    public static func from(_ raw: UInt32) -> TtsErrorCode {
        TtsErrorCode(rawValue: raw) ?? .unknown
    }
}

/// Structured error detail (GH #69): the message is secret-redacted.
public struct TtsErrorDetail: Sendable, Hashable {
    public let code: UInt32
    public let codeName: String
    public let message: String
    public let displayMessage: String
    public let engineId: String
    public let fieldKey: String
}

/// Tri-state credential check outcome (GH #69): static catalogs answer
/// `unverified` — enumerating them proves nothing about the key.
public enum CredentialStatus: String, Sendable {
    case verified
    case rejected
    case unverified
}

/// Result of `TtsClient.checkCredentials()`.
public enum CredentialCheckResult: Sendable {
    case verified
    case rejected
    case unverified
    /// The check itself failed (missing field, unreachable provider) —
    /// carries the structured detail.
    case checkFailed(TtsErrorDetail)
}

/// Control kind for a setup field (GH #68). Unknown kinds from newer
/// native builds decode as `.text`.
public enum TtsSetupFieldKind: String, Sendable {
    case secret
    case filePath
    case directoryPath
    case url
    case choice
    case number
    case text
}

/// One typed setup/credential field (GH #68).
public struct TtsSetupField: Sendable, Hashable {
    public let key: String
    public let label: String
    public let kind: TtsSetupFieldKind
    public let required: Bool
    public let defaultValue: String?
    public let choices: [String]
    public let placeholder: String?
    public let helpText: String?
}

/// Engine capability flags (GH #68).
public struct TtsEngineCapabilities: Sendable, Hashable {
    public let supportsRate: Bool
    public let supportsPitch: Bool
    public let supportsVolume: Bool
    public let supportsWordTiming: Bool
    public let supportsSsml: Bool
    public let supportsStreaming: Bool
    public let requiresInternet: Bool
    /// Voice enumeration succeeds without authentication — listing
    /// alone is not credential validation.
    public let staticVoiceCatalog: Bool
    public let notes: String?
}

/// Full setup schema for one engine (GH #68).
public struct TtsEngineSetup: Sendable, Hashable {
    public let id: String
    public let name: String
    public let needsCredentials: Bool
    public let fields: [TtsSetupField]
    public let capabilities: TtsEngineCapabilities
    /// Either/or groups: at least one field in each group is required.
    public let requiredAny: [[String]]
}

/// Strongly-typed closure aliases.
public typealias AudioCallback = @Sendable (Data) -> Void
/// - Parameters:
///   - word: the word being spoken
///   - charOffset/charLen: source-text position (-1 when unknown)
///   - start/end: audio position in seconds
///   - estimated: true = proportional estimate; false = measured timings
public typealias BoundaryCallback = @Sendable (
    _ word: String, _ charOffset: Int32, _ charLen: Int32,
    _ start: Double, _ end: Double, _ estimated: Bool
) -> Void
public typealias MarkCallback = @Sendable (
    _ name: String, _ charOffset: Int32, _ start: Double, _ end: Double
) -> Void
public typealias VisemeCallback = @Sendable (_ visemeId: Int32, _ offsetSec: Double) -> Void
public typealias LifecycleCallback = @Sendable () -> Void
public typealias ErrorCallback = @Sendable (_ error: String) -> Void

/// High-level Swift client for rust-tts-wrapper.
///
/// Wraps a single engine instance (`tts_ctx *` on the C side) and exposes it
/// as a `Sendable` value type backed by a class for shared mutable state.
public final class TtsClient: @unchecked Sendable {
    private var ctx: OpaquePointer?
    // Strong refs to the boxed callback contexts so the C function pointer
    // remains valid for as long as the client lives.
    private var audioBox: CallbackBox<AudioCallback>?
    private var boundaryBox: CallbackBox<BoundaryCallback>?
    private var markBox: CallbackBox<MarkCallback>?
    private var visemeBox: CallbackBox<VisemeCallback>?
    private var startBox: CallbackBox<LifecycleCallback>?
    private var endBox: CallbackBox<LifecycleCallback>?
    private var errorBox: CallbackBox<ErrorCallback>?

    public init(engineId: String = "system", credentials: [String: String] = [:]) throws {
        let credsJson = (try? JSONSerialization.data(withJSONObject: credentials))
            .flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        ctx = engineId.withCString { enginePtr in
            credsJson.withCString { credsPtr in
                tts_create(enginePtr, credsPtr)
            }
        }
        if ctx == nil {
            // GH #69: carry the structured create-time detail when the
            // native build provides it.
            let detail = Self.getGlobalLastErrorDetail()
            let fallback = "Failed to create engine '\(engineId)': \(Self.getGlobalLastError() ?? "unknown")"
            throw TtsError(fallback, detail: detail)
        }
    }

    deinit { close() }

    public func close() {
        if let ctx { tts_destroy(ctx) }
        ctx = nil
    }

    // --- synthesis -----------------------------------------------------

    /// Speak asynchronously (engine-defined).
    public func speak(_ text: String) throws {
        guard let ctx else { throw TtsError("client closed") }
        let rc = text.withCString { tts_speak(ctx, $0) }
        if rc != 0 { throw structuredError("speak failed") }
    }

    /// Speak pre-built SSML (GH #73). Prosody wrapping is bypassed —
    /// callers embed rate/pitch/volume themselves; engines that parse
    /// no SSML get it translated to their model-matched dialect.
    public func speakSsml(_ ssml: String) throws {
        guard let ctx else { throw TtsError("client closed") }
        let rc = ssml.withCString { tts_speak_ssml(ctx, $0) }
        if rc != 0 { throw structuredError("speak_ssml failed") }
    }

    /// Speak synchronously (block until done).
    public func speakSync(_ text: String) throws {
        guard let ctx else { throw TtsError("client closed") }
        let rc = text.withCString { tts_speak_sync(ctx, $0) }
        if rc != 0 { throw structuredError("speak_sync failed") }
    }

    /// Synthesise to a byte buffer.
    public func synthToBytes(_ text: String) throws -> Data {
        guard let ctx else { throw TtsError("client closed") }
        var bufPtr: UnsafeMutablePointer<UInt8>?
        var length: UInt = 0
        let rc = text.withCString { textPtr in
            tts_synth_to_bytes(ctx, textPtr, &bufPtr, &length)
        }
        if rc != 0 { throw structuredError("synth_to_bytes failed") }
        guard let buf = bufPtr, length > 0 else { return Data() }
        defer { tts_free_bytes(buf, length) }
        return Data(bytes: buf, count: Int(length))
    }

    // --- playback control ---------------------------------------------

    public func stop() {
        guard let ctx else { return }
        tts_stop(ctx)
    }

    public func pause() {
        guard let ctx else { return }
        tts_pause(ctx)
    }

    public func resume() {
        guard let ctx else { return }
        tts_resume(ctx)
    }

    // --- per-instance settings ----------------------------------------

    public func setVoice(_ voiceId: String) {
        guard let ctx else { return }
        voiceId.withCString { tts_set_voice(ctx, $0) }
    }
    public func setRate(_ rate: Float) {
        guard let ctx else { return }
        tts_set_rate(ctx, rate)
    }
    public func setPitch(_ pitch: Float) {
        guard let ctx else { return }
        tts_set_pitch(ctx, pitch)
    }
    public func setVolume(_ volume: Float) {
        guard let ctx else { return }
        tts_set_volume(ctx, volume)
    }

    // --- callbacks -----------------------------------------------------

    /// Register a streaming-audio callback. Pass `nil` to clear.
    public func setOnAudio(_ callback: AudioCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            audioBox = box
            // Bridge a plain C entry point back into the Swift closure.
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            tts_set_on_audio(
                ctx,
                { bytes, len, userdata in
                    guard let bytes, len > 0, let userdata else { return }
                    let box = Unmanaged<CallbackBox<AudioCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    let data = Data(bytes: bytes, count: Int(len))
                    box.callback(data)
                },
                opaque
            )
        } else {
            audioBox = nil
            tts_set_on_audio(ctx, nil, nil)
        }
    }

    /// Register a word-boundary callback. Pass `nil` to clear.
    public func setOnBoundary(_ callback: BoundaryCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            boundaryBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            tts_set_on_boundary(
                ctx,
                { wordPtr, charOffset, charLen, start, end, estimated, userdata in
                    guard let userdata else { return }
                    let box = Unmanaged<CallbackBox<BoundaryCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    let word = wordPtr.map { String(cString: $0) } ?? ""
                    box.callback(word, charOffset, charLen, Double(start), Double(end), estimated != 0)
                },
                opaque
            )
        } else {
            boundaryBox = nil
            tts_set_on_boundary(ctx, nil, nil)
        }
    }

    /// Register a mark/bookmark callback (SSML `<mark>`). Pass `nil` to clear.
    public func setOnMark(_ callback: MarkCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            markBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            tts_set_on_mark(
                ctx,
                { namePtr, charOffset, start, end, userdata in
                    guard let userdata else { return }
                    let box = Unmanaged<CallbackBox<MarkCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    let name = namePtr.map { String(cString: $0) } ?? ""
                    box.callback(name, charOffset, Double(start), Double(end))
                },
                opaque
            )
        } else {
            markBox = nil
            tts_set_on_mark(ctx, nil, nil)
        }
    }

    /// Register a viseme callback for lip-sync. Pass `nil` to clear.
    public func setOnViseme(_ callback: VisemeCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            visemeBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            tts_set_on_viseme(
                ctx,
                { visemeId, offsetSec, userdata in
                    guard let userdata else { return }
                    let box = Unmanaged<CallbackBox<VisemeCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    box.callback(visemeId, Double(offsetSec))
                },
                opaque
            )
        } else {
            visemeBox = nil
            tts_set_on_viseme(ctx, nil, nil)
        }
    }

    /// Register a speech-started callback. Pass `nil` to clear.
    public func setOnStart(_ callback: LifecycleCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            startBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            tts_set_on_start(
                ctx,
                { userdata in
                    guard let userdata else { return }
                    let box = Unmanaged<CallbackBox<LifecycleCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    box.callback()
                },
                opaque
            )
        } else {
            startBox = nil
            tts_set_on_start(ctx, nil, nil)
        }
    }

    /// Register a speech-completed callback. Pass `nil` to clear.
    public func setOnEnd(_ callback: LifecycleCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            endBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            tts_set_on_end(
                ctx,
                { userdata in
                    guard let userdata else { return }
                    let box = Unmanaged<CallbackBox<LifecycleCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    box.callback()
                },
                opaque
            )
        } else {
            endBox = nil
            tts_set_on_end(ctx, nil, nil)
        }
    }

    /// Register an error callback. Pass `nil` to clear.
    public func setOnError(_ callback: ErrorCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            errorBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            tts_set_on_error(
                ctx,
                { errorPtr, userdata in
                    guard let userdata else { return }
                    let box = Unmanaged<CallbackBox<ErrorCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    let msg = errorPtr.map { String(cString: $0) } ?? "unknown error"
                    box.callback(msg)
                },
                opaque
            )
        } else {
            errorBox = nil
            tts_set_on_error(ctx, nil, nil)
        }
    }

    // --- enumeration ---------------------------------------------------

    /// List the voices installed on this engine.
    public func getVoices() throws -> [TtsVoice] {
        guard let ctx else { throw TtsError("client closed") }
        var arr: UnsafeMutablePointer<TtsVoiceC>?
        var count: Int32 = 0
        let rc = tts_get_voices(ctx, &arr, &count)
        if rc != 0 { throw structuredError("get_voices failed") }
        guard let arr, count > 0 else { return [] }
        defer { tts_free_voices(arr, count) }

        var voices: [TtsVoice] = []
        voices.reserveCapacity(Int(count))
        for i in 0..<Int(count) {
            let v = arr[i]
            voices.append(TtsVoice(
                id: cStringPtr(v.id),
                name: cStringPtr(v.name),
                language: cStringPtr(v.language),
                gender: cStringPtr(v.gender),
                engine: cStringPtr(v.engine)
            ))
        }
        return voices
    }

    /// List all engines compiled into this build.
    public static func listEngines() throws -> [TtsEngineInfo] {
        var arr: UnsafeMutablePointer<TtsEngineInfoC>?
        var count: Int32 = 0
        let rc = tts_get_engines(&arr, &count)
        if rc != 0 {
            throw TtsError(getGlobalLastError() ?? "tts_get_engines failed")
        }
        guard let arr, count > 0 else { return [] }
        defer { tts_free_engines(arr, count) }

        var engines: [TtsEngineInfo] = []
        engines.reserveCapacity(Int(count))
        for i in 0..<Int(count) {
            let e = arr[i]
            let keysJson = cStringPtr(e.credential_keys_json)
            let keys = (try? JSONSerialization.jsonObject(with: Data(keysJson.utf8))) as? [String] ?? []
            engines.append(TtsEngineInfo(
                id: cStringPtr(e.id),
                name: cStringPtr(e.name),
                needsCredentials: e.needs_credentials,
                credentialKeys: keys
            ))
        }
        return engines
    }

    /// Number of engines available. Convenience over `listEngines()`.
    public static func engineCount() -> Int {
        Int(tts_get_engine_count())
    }

    // --- error handling ------------------------------------------------

    /// Last error for this context, or `nil` if none.
    public func getLastError() -> String? {
        guard let ctx else { return nil }
        guard let ptr = tts_get_last_error(ctx) else { return nil }
        return String(cString: ptr)
    }

    /// Stable classification of the last error (`.unknown` = none, GH #69).
    public func getLastErrorCode() -> TtsErrorCode {
        guard let ctx else { return .unknown }
        return TtsErrorCode.from(tts_get_last_error_code(ctx))
    }

    /// Structured detail of the last error, or `nil` (GH #69). The
    /// message is secret-redacted; `fieldKey` names the offending
    /// settings row.
    public func getLastErrorDetail() -> TtsErrorDetail? {
        guard let ctx else { return nil }
        guard let ptr = tts_get_last_error_detail_json(ctx) else { return nil }
        defer { tts_free_string(ptr) }
        return Self.decodeDetail(String(cString: ptr))
    }

    /// Tri-state credential check (GH #69). Static catalogs answer
    /// `.unverified` — enumerating them proves nothing about the key.
    public func checkCredentials() -> CredentialCheckResult {
        guard let ctx else { return .checkFailed(TtsErrorDetail(
            code: 0, codeName: "Unknown", message: "client closed",
            displayMessage: "", engineId: "", fieldKey: "")) }
        switch tts_check_credentials(ctx) {
        case 1: return .verified
        case 0: return .rejected
        case 2: return .unverified
        default: return .checkFailed(
            getLastErrorDetail()
                ?? TtsErrorDetail(code: 0, codeName: "Unknown", message: getLastError() ?? "",
                                  displayMessage: "", engineId: "", fieldKey: ""))
        }
    }

    /// Global last error (used when no context exists, e.g. `tts_create`).
    public static func getGlobalLastError() -> String? {
        guard let ptr = tts_get_last_error(nil) else { return nil }
        return String(cString: ptr)
    }

    /// Stable code of the global (create-time) error (GH #69).
    public static func getGlobalLastErrorCode() -> TtsErrorCode {
        TtsErrorCode.from(tts_get_last_error_code(nil))
    }

    /// Structured detail of the global error, or `nil` (GH #69).
    public static func getGlobalLastErrorDetail() -> TtsErrorDetail? {
        guard let ptr = tts_get_last_error_detail_json(nil) else { return nil }
        defer { tts_free_string(ptr) }
        return decodeDetail(String(cString: ptr))
    }

    // --- engine setup schema (GH #68) -----------------------------------

    /// Typed setup schema for every engine compiled into this build:
    /// fields with kinds (secret / filePath / directoryPath / url /
    /// choice / number / text) plus capability flags, so a settings UI
    /// renders engine rows without a provider switch. Unknown kinds
    /// from newer native builds degrade to `.text`.
    public static func engineSetups() throws -> [TtsEngineSetup] {
        guard let ptr = tts_get_engine_setups_json() else {
            throw TtsError("tts_get_engine_setups_json returned null")
        }
        defer { tts_free_string(ptr) }
        let json = String(cString: ptr)
        guard let data = json.data(using: .utf8),
              let arr = try JSONSerialization.jsonObject(with: data) as? [[String: Any]] else {
            throw TtsError("engine setup JSON failed to parse")
        }
        return arr.compactMap(Self.decodeSetup)
    }

    /// The setup schema for one engine id, or `nil` when unknown.
    public static func engineSetup(_ engineId: String) throws -> TtsEngineSetup? {
        try engineSetups().first { $0.id == engineId }
    }

    // --- private helpers ----------------------------------------------

    /// Build a `TtsError` carrying the structured detail (GH #69).
    private func structuredError(_ fallback: String) -> TtsError {
        TtsError(getLastError() ?? fallback, detail: getLastErrorDetail())
    }

    /// Decode the detail JSON defensively (nil on malformed input).
    private static func decodeDetail(_ json: String) -> TtsErrorDetail? {
        guard let data = json.data(using: .utf8),
              let d = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return nil
        }
        return TtsErrorDetail(
            code: (d["code"] as? NSNumber)?.uint32Value ?? 0,
            codeName: d["name"] as? String ?? "Unknown",
            message: d["message"] as? String ?? "",
            displayMessage: d["displayMessage"] as? String ?? "",
            engineId: d["engineId"] as? String ?? "",
            fieldKey: d["fieldKey"] as? String ?? ""
        )
    }

    /// Decode one engine-setup object tolerantly (unknown kinds → text,
    /// missing properties → defaults).
    private static func decodeSetup(_ s: [String: Any]) -> TtsEngineSetup? {
        guard let id = s["id"] as? String else { return nil }
        let caps = s["capabilities"] as? [String: Any] ?? [:]
        let fields = (s["fields"] as? [[String: Any]] ?? []).map { f -> TtsSetupField in
            TtsSetupField(
                key: f["key"] as? String ?? "",
                label: f["label"] as? String ?? "",
                kind: TtsSetupFieldKind(rawValue: f["kind"] as? String ?? "") ?? .text,
                required: f["required"] as? Bool ?? false,
                defaultValue: f["defaultValue"] as? String,
                choices: (f["choices"] as? [String]) ?? [],
                placeholder: f["placeholder"] as? String,
                helpText: f["helpText"] as? String
            )
        }
        let requiredAny = (s["requiredAny"] as? [[String]]) ?? []
        return TtsEngineSetup(
            id: id,
            name: s["name"] as? String ?? "",
            needsCredentials: s["needsCredentials"] as? Bool ?? false,
            fields: fields,
            capabilities: TtsEngineCapabilities(
                supportsRate: caps["supportsRate"] as? Bool ?? false,
                supportsPitch: caps["supportsPitch"] as? Bool ?? false,
                supportsVolume: caps["supportsVolume"] as? Bool ?? false,
                supportsWordTiming: caps["supportsWordTiming"] as? Bool ?? false,
                supportsSsml: caps["supportsSsml"] as? Bool ?? false,
                supportsStreaming: caps["supportsStreaming"] as? Bool ?? false,
                requiresInternet: caps["requiresInternet"] as? Bool ?? false,
                staticVoiceCatalog: caps["staticVoiceCatalog"] as? Bool ?? false,
                notes: caps["notes"] as? String
            ),
            requiredAny: requiredAny
        )
    }

}

private func cStringPtr(_ p: UnsafePointer<CChar>?) -> String {
    p.map { String(cString: $0) } ?? ""
}

/// Type-erased container that holds a Swift closure alive while the C code
/// holds a raw pointer to it via `Unmanaged`.
private final class CallbackBox<T> {
    let callback: T
    init(_ callback: T) { self.callback = callback }
}

// C-side structs that mirror `tts_voice` / `tts_engine_info` from
// `include/tts_wrapper.h`. These are imported automatically when the Swift
// module links against the rust-tts-wrapper staticlib and imports the C
// header; we declare fallback aliases here so the file compiles even when
// the import isn't found.
#if !canImport(CRustTtsWrapper) && !canImport(rust_tts_wrapper)
@_cdecl("tts_create") public func ttsCreate(_ engineId: UnsafePointer<CChar>?, _ creds: UnsafePointer<CChar>?) -> OpaquePointer? { nil }
@_cdecl("tts_destroy") public func ttsDestroy(_ ctx: OpaquePointer?) {}
@_cdecl("tts_speak") public func ttsSpeak(_ ctx: OpaquePointer?, _ text: UnsafePointer<CChar>?) -> Int32 { -1 }
@_cdecl("tts_speak_sync") public func ttsSpeakSync(_ ctx: OpaquePointer?, _ text: UnsafePointer<CChar>?) -> Int32 { -1 }
@_cdecl("tts_stop") public func ttsStop(_ ctx: OpaquePointer?) {}
@_cdecl("tts_pause") public func ttsPause(_ ctx: OpaquePointer?) {}
@_cdecl("tts_resume") public func ttsResume(_ ctx: OpaquePointer?) {}
@_cdecl("tts_synth_to_bytes") public func ttsSynthToBytes(_ ctx: OpaquePointer?, _ text: UnsafePointer<CChar>?, _ buf: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?, _ len: UnsafeMutablePointer<Int>?) -> Int32 { -1 }
@_cdecl("tts_free_bytes") public func ttsFreeBytes(_ buf: UnsafeMutablePointer<UInt8>?, _ len: Int) {}
@_cdecl("tts_set_voice") public func ttsSetVoice(_ ctx: OpaquePointer?, _ voiceId: UnsafePointer<CChar>?) {}
@_cdecl("tts_set_rate") public func ttsSetRate(_ ctx: OpaquePointer?, _ rate: Float) {}
@_cdecl("tts_set_pitch") public func ttsSetPitch(_ ctx: OpaquePointer?, _ pitch: Float) {}
@_cdecl("tts_set_volume") public func ttsSetVolume(_ ctx: OpaquePointer?, _ volume: Float) {}
@_cdecl("tts_set_on_audio") public func ttsSetOnAudio(_ ctx: OpaquePointer?, _ cb: UnsafeRawPointer?, _ userdata: UnsafeRawPointer?) {}
@_cdecl("tts_set_on_boundary") public func ttsSetOnBoundary(_ ctx: OpaquePointer?, _ cb: UnsafeRawPointer?, _ userdata: UnsafeRawPointer?) {}
@_cdecl("tts_get_voices") public func ttsGetVoices(_ ctx: OpaquePointer?, _ voices: UnsafeMutablePointer<UnsafeMutablePointer<TtsVoiceC>?>?, _ count: UnsafeMutablePointer<Int32>?) -> Int32 { -1 }
@_cdecl("tts_free_voices") public func ttsFreeVoices(_ voices: UnsafeMutablePointer<TtsVoiceC>?, _ count: Int32) {}
@_cdecl("tts_get_engines") public func ttsGetEngines(_ engines: UnsafeMutablePointer<UnsafeMutablePointer<TtsEngineInfoC>?>?, _ count: UnsafeMutablePointer<Int32>?) -> Int32 { -1 }
@_cdecl("tts_free_engines") public func ttsFreeEngines(_ engines: UnsafeMutablePointer<TtsEngineInfoC>?, _ count: Int32) {}
@_cdecl("tts_get_engine_count") public func ttsGetEngineCount() -> Int32 { 0 }
@_cdecl("tts_get_last_error") public func ttsGetLastError(_ ctx: OpaquePointer?) -> UnsafePointer<CChar>? { nil }

public struct TtsVoiceC {
    public var id: UnsafePointer<CChar>?
    public var name: UnsafePointer<CChar>?
    public var language: UnsafePointer<CChar>?
    public var gender: UnsafePointer<CChar>?
    public var engine: UnsafePointer<CChar>?
    public init() {}
}

public struct TtsEngineInfoC {
    public var id: UnsafePointer<CChar>?
    public var name: UnsafePointer<CChar>?
    public var needs_credentials: Bool
    public var credential_keys_json: UnsafePointer<CChar>?
    public init() {}
}
#endif
