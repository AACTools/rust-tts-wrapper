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
    [DllImport("rust_tts_wrapper")] public static extern void tts_pause(IntPtr ctx);
    [DllImport("rust_tts_wrapper")] public static extern void tts_resume(IntPtr ctx);
    [DllImport("rust_tts_wrapper")] public static extern int tts_synth_to_bytes(IntPtr ctx, string text, out IntPtr outBytes, out UIntPtr outLen);
    [DllImport("rust_tts_wrapper")] public static extern void tts_free_bytes(IntPtr bytes, UIntPtr len);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_voice(IntPtr ctx, string voiceId);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_rate(IntPtr ctx, float rate);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_pitch(IntPtr ctx, float pitch);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_volume(IntPtr ctx, float volume);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_on_audio(IntPtr ctx, IntPtr cb, IntPtr userdata);
    [DllImport("rust_tts_wrapper")] public static extern void tts_set_on_boundary(IntPtr ctx, IntPtr cb, IntPtr userdata);
    [DllImport("rust_tts_wrapper")] public static extern int tts_get_voices(IntPtr ctx, out IntPtr voices, out int count);
    [DllImport("rust_tts_wrapper")] public static extern void tts_free_voices(IntPtr voices, int count);
    [DllImport("rust_tts_wrapper")] public static extern int tts_get_engine_count();
    [DllImport("rust_tts_wrapper")] public static extern IntPtr tts_get_last_error();
}

public delegate void AudioCallback(byte[] chunk);
public delegate void BoundaryCallback(string word, float startTime, float endTime);

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
    public void Pause() => Native.tts_pause(_ctx);
    public void Resume() => Native.tts_resume(_ctx);

    public byte[] SynthToBytes(string text)
    {
        IntPtr bufPtr;
        UIntPtr len;
        int result = Native.tts_synth_to_bytes(_ctx, text, out bufPtr, out len);
        if (result != 0) throw new InvalidOperationException("Synthesis to bytes failed");
        byte[] data = new byte[len.ToUInt32()];
        if (data.Length > 0) Marshal.Copy(bufPtr, data, 0, data.Length);
        Native.tts_free_bytes(bufPtr, len);
        return data;
    }

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
