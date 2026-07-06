import Foundation
// Importing the C header that cbindgen generates at build time. When you
// embed this file into an Xcode / SwiftPM project that also builds the Rust
// staticlib, the module name matches the crate name (`rust_tts_wrapper`).
#if canImport(rust_tts_wrapper)
import rust_tts_wrapper
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
/// `tts_get_last_error` on the owning context.
public struct TtsError: Error, CustomStringConvertible {
    public let message: String
    public init(_ message: String) { self.message = message }
    public var description: String { message }
}

/// Strongly-typed closure aliases.
public typealias AudioCallback = @Sendable (Data) -> Void
public typealias BoundaryCallback = @Sendable (_ word: String, _ start: Double, _ end: Double) -> Void
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
    private var startBox: CallbackBox<LifecycleCallback>?
    private var endBox: CallbackBox<LifecycleCallback>?
    private var errorBox: CallbackBox<ErrorCallback>?

    public init(engineId: String = "system", credentials: [String: String] = [:]) throws {
        let credsJson = (try? JSONSerialization.data(withJSONObject: credentials))
            .flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        ctx = engineId.withCString { enginePtr in
            credsJson.withCString { credsPtr in
                rust_tts_wrapper.tts_create(enginePtr, credsPtr)
            }
        }
        if ctx == nil {
            throw TtsError("Failed to create engine '\(engineId)': \(Self.getGlobalLastError() ?? "unknown")")
        }
    }

    deinit { close() }

    public func close() {
        if let ctx { rust_tts_wrapper.tts_destroy(ctx) }
        ctx = nil
    }

    // --- synthesis -----------------------------------------------------

    /// Speak asynchronously (engine-defined).
    public func speak(_ text: String) throws {
        guard let ctx else { throw TtsError("client closed") }
        let rc = text.withCString { rust_tts_wrapper.tts_speak(ctx, $0) }
        if rc != 0 { throw TtsError(getLastError() ?? "speak failed") }
    }

    /// Speak synchronously (block until done).
    public func speakSync(_ text: String) throws {
        guard let ctx else { throw TtsError("client closed") }
        let rc = text.withCString { rust_tts_wrapper.tts_speak_sync(ctx, $0) }
        if rc != 0 { throw TtsError(getLastError() ?? "speak_sync failed") }
    }

    /// Synthesise to a byte buffer.
    public func synthToBytes(_ text: String) throws -> Data {
        guard let ctx else { throw TtsError("client closed") }
        var bufPtr: UnsafeMutablePointer<UInt8>?
        var length: Int = 0
        let rc = text.withCString { textPtr in
            rust_tts_wrapper.tts_synth_to_bytes(ctx, textPtr, &bufPtr, &length)
        }
        if rc != 0 { throw TtsError(getLastError() ?? "synth_to_bytes failed") }
        guard let buf = bufPtr, length > 0 else { return Data() }
        defer { rust_tts_wrapper.tts_free_bytes(buf, length) }
        return Data(bytes: buf, count: length)
    }

    // --- playback control ---------------------------------------------

    public func stop() {
        guard let ctx else { return }
        rust_tts_wrapper.tts_stop(ctx)
    }

    public func pause() {
        guard let ctx else { return }
        rust_tts_wrapper.tts_pause(ctx)
    }

    public func resume() {
        guard let ctx else { return }
        rust_tts_wrapper.tts_resume(ctx)
    }

    // --- per-instance settings ----------------------------------------

    public func setVoice(_ voiceId: String) {
        guard let ctx else { return }
        voiceId.withCString { rust_tts_wrapper.tts_set_voice(ctx, $0) }
    }
    public func setRate(_ rate: Float) {
        guard let ctx else { return }
        rust_tts_wrapper.tts_set_rate(ctx, rate)
    }
    public func setPitch(_ pitch: Float) {
        guard let ctx else { return }
        rust_tts_wrapper.tts_set_pitch(ctx, pitch)
    }
    public func setVolume(_ volume: Float) {
        guard let ctx else { return }
        rust_tts_wrapper.tts_set_volume(ctx, volume)
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
            rust_tts_wrapper.tts_set_on_audio(
                ctx,
                { bytes, len, userdata in
                    guard let bytes, len > 0, let userdata else { return }
                    let box = Unmanaged<CallbackBox<AudioCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    let data = Data(bytes: bytes, count: len)
                    box.callback(data)
                },
                opaque
            )
        } else {
            audioBox = nil
            rust_tts_wrapper.tts_set_on_audio(ctx, nil, nil)
        }
    }

    /// Register a word-boundary callback. Pass `nil` to clear.
    public func setOnBoundary(_ callback: BoundaryCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            boundaryBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            rust_tts_wrapper.tts_set_on_boundary(
                ctx,
                { wordPtr, start, end, userdata in
                    guard let userdata else { return }
                    let box = Unmanaged<CallbackBox<BoundaryCallback>>.fromOpaque(userdata).takeUnretainedValue()
                    let word = wordPtr.map { String(cString: $0) } ?? ""
                    box.callback(word, Double(start), Double(end))
                },
                opaque
            )
        } else {
            boundaryBox = nil
            rust_tts_wrapper.tts_set_on_boundary(ctx, nil, nil)
        }
    }

    /// Register a speech-started callback. Pass `nil` to clear.
    public func setOnStart(_ callback: LifecycleCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            startBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            rust_tts_wrapper.tts_set_on_start(
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
            rust_tts_wrapper.tts_set_on_start(ctx, nil, nil)
        }
    }

    /// Register a speech-completed callback. Pass `nil` to clear.
    public func setOnEnd(_ callback: LifecycleCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            endBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            rust_tts_wrapper.tts_set_on_end(
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
            rust_tts_wrapper.tts_set_on_end(ctx, nil, nil)
        }
    }

    /// Register an error callback. Pass `nil` to clear.
    public func setOnError(_ callback: ErrorCallback?) {
        guard let ctx else { return }
        if let callback {
            let box = CallbackBox(callback)
            errorBox = box
            let opaque = Unmanaged.passUnretained(box).toOpaque()
            rust_tts_wrapper.tts_set_on_error(
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
            rust_tts_wrapper.tts_set_on_error(ctx, nil, nil)
        }
    }

    // --- enumeration ---------------------------------------------------

    /// List the voices installed on this engine.
    public func getVoices() throws -> [TtsVoice] {
        guard let ctx else { throw TtsError("client closed") }
        var arr: UnsafeMutablePointer<TtsVoiceC>?
        var count: Int32 = 0
        let rc = rust_tts_wrapper.tts_get_voices(ctx, &arr, &count)
        if rc != 0 { throw TtsError(getLastError() ?? "get_voices failed") }
        guard let arr, count > 0 else { return [] }
        defer { rust_tts_wrapper.tts_free_voices(arr, count) }

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
        let rc = rust_tts_wrapper.tts_get_engines(&arr, &count)
        if rc != 0 {
            throw TtsError(getGlobalLastError() ?? "tts_get_engines failed")
        }
        guard let arr, count > 0 else { return [] }
        defer { rust_tts_wrapper.tts_free_engines(arr, count) }

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
        Int(rust_tts_wrapper.tts_get_engine_count())
    }

    // --- error handling ------------------------------------------------

    /// Last error for this context, or `nil` if none.
    public func getLastError() -> String? {
        guard let ctx else { return nil }
        guard let ptr = rust_tts_wrapper.tts_get_last_error(ctx) else { return nil }
        return String(cString: ptr)
    }

    /// Global last error (used when no context exists, e.g. `tts_create`).
    public static func getGlobalLastError() -> String? {
        guard let ptr = rust_tts_wrapper.tts_get_last_error(nil) else { return nil }
        return String(cString: ptr)
    }

    // --- private helpers ----------------------------------------------

    private func cStringPtr(_ p: UnsafePointer<CChar>?) -> String {
        p.map { String(cString: $0) } ?? ""
    }
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
#if !canImport(rust_tts_wrapper)
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
