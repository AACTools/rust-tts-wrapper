using System;
using System.Runtime.InteropServices;
using System.Text.Json;

namespace TtsWrapper;

public static class Native
{
    [DllImport("rust_tts_wrapper")] public static extern IntPtr tts_create(string engineId, string credentialsJson);
    [DllImport("rust_tts_wrapper")] public static extern void tts_destroy(IntPtr ctx);
    [DllImport("rust_tts_wrapper")] public static extern int tts_speak(IntPtr ctx, string text);
    [DllImport("rust_tts_wrapper")] public static extern int tts_speak_sync(IntPtr ctx, string text);
    [DllImport("rust_tts_wrapper")] public static extern void tts_stop(IntPtr ctx);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_voice(IntPtr ctx, string voiceId);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_rate(IntPtr ctx, float rate);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_pitch(IntPtr ctx, float pitch);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_volume(IntPtr ctx, float volume);
    [DllImport("rust_tts_wrapper")] public static extern int tts_get_engine_count();
}

public class TtsClient : IDisposable
{
    private IntPtr _ctx;
    private bool _disposed;

    public TtsClient(string engineId = "system", Dictionary<string, string>? credentials = null)
    {
        var json = JsonSerializer.Serialize(credentials ?? new Dictionary<string, string>());
        _ctx = Native.tts_create(engineId, json);
        if (_ctx == IntPtr.Zero)
            throw new InvalidOperationException("Failed to create TTS engine");
    }

    public void Speak(string text) =>
        Native.tts_speak(_ctx, text);

    public void SpeakSync(string text) =>
        Native.tts_speak_sync(_ctx, text);

    public void Stop() => Native.tts_stop(_ctx);
    public void SetVoice(string voiceId) => Native.tts_set_voice(_ctx, voiceId);
    public void SetRate(float rate) => Native.tts_set_rate(_ctx, rate);
    public void SetPitch(float pitch) => Native.tts_set_pitch(_ctx, pitch);
    public void SetVolume(float volume) => Native.tts_set_volume(_ctx, volume);

    public void Dispose()
    {
        if (!_disposed)
        {
            if (_ctx != IntPtr.Zero) Native.tts_destroy(_ctx);
            _disposed = true;
        }
        GC.SuppressFinalize(this);
    }

    ~TtsClient() => Dispose();
}
