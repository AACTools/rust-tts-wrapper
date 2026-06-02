import rust_tts_wrapper

@_cdecl("tts_create")
public func ttsCreate(_ engineId: UnsafePointer<CChar>?, _ credentialsJson: UnsafePointer<CChar>?) -> OpaquePointer? {
    guard let engineId else { return nil }
    let creds = credentialsJson.map { String(cString: $0) } ?? ""
    return rust_tts_wrapper.tts_create(engineId, creds)
}

@_cdecl("tts_destroy")
public func ttsDestroy(_ ctx: OpaquePointer?) {
    guard let ctx else { return }
    rust_tts_wrapper.tts_destroy(ctx)
}

@_cdecl("tts_speak")
public func ttsSpeak(_ ctx: OpaquePointer?, _ text: UnsafePointer<CChar>?) -> Int32 {
    guard let ctx, let text else { return -1 }
    return rust_tts_wrapper.tts_speak(ctx, text)
}

@_cdecl("tts_speak_sync")
public func ttsSpeakSync(_ ctx: OpaquePointer?, _ text: UnsafePointer<CChar>?) -> Int32 {
    guard let ctx, let text else { return -1 }
    return rust_tts_wrapper.tts_speak_sync(ctx, text)
}

public class TTSClient {
    private var ctx: OpaquePointer?

    public init(engineId: String = "system", credentials: [String: String] = [:]) {
        let credsJson = (try? JSONSerialization.data(withJSONObject: credentials))
            .flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        ctx = ttsCreate(engineId, credsJson)
    }

    deinit {
        ttsDestroy(ctx)
    }

    public func speak(_ text: String) {
        text.withCString { ptr in
            _ = ttsSpeak(ctx, ptr)
        }
    }

    public func speakSync(_ text: String) {
        text.withCString { ptr in
            _ = ttsSpeakSync(ctx, ptr)
        }
    }

    public func stop() {
        guard let ctx else { return }
        rust_tts_wrapper.tts_stop(ctx)
    }

    public func setVoice(_ voiceId: String) {
        guard let ctx else { return }
        voiceId.withCString { ptr in
            rust_tts_wrapper.tts_set_voice(ctx, ptr)
        }
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
}
