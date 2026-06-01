import rust_tts_wrapper

@_cdecl("tts_create")
public func ttsCreate(_ engineId: UnsafePointer<CChar>?, _ credentialsJson: UnsafePointer<CChar>?) -> OpaquePointer? {
    guard let engineId else { return nil }
    let creds = credentialsJson.map { String(cString: $0) } ?? ""
    let ctx = rust_tts_wrapper.tts_create(engineId, creds)
    return ctx
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

public class TTSClient {
    private var ctx: OpaquePointer?

    public init(engineId: String = "system", credentials: [String: String] = [:]) {
        let credsJson = try? JSONSerialization.data(withJSONObject: credentials)
        let credsStr = credsJson.flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        ctx = ttsCreate(engineId, credsStr)
    }

    deinit {
        ttsDestroy(ctx)
    }

    public func speak(_ text: String) {
        text.withCString { ptr in
            _ = ttsSpeak(ctx, ptr)
        }
    }
}
