using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace RustTtsWrapper;

/// <summary>
/// Native P/Invoke declarations for the rust-tts-wrapper C ABI.
/// </summary>
public static class Native
{
    private const string Lib = "rust_tts_wrapper";

    [DllImport(Lib)] public static extern IntPtr tts_create(string engineId, string credentialsJson);
    [DllImport(Lib)] public static extern void tts_destroy(IntPtr ctx);
    [DllImport(Lib)] public static extern int tts_speak(IntPtr ctx, string text);
    [DllImport(Lib)] public static extern int tts_speak_sync(IntPtr ctx, string text);
    [DllImport(Lib)] public static extern void tts_stop(IntPtr ctx);
    [DllImport(Lib)] public static extern void tts_pause(IntPtr ctx);
    [DllImport(Lib)] public static extern void tts_resume(IntPtr ctx);

    [DllImport(Lib)] public static extern int tts_synth_to_bytes(IntPtr ctx, string text, out IntPtr outBytes, out UIntPtr outLen);
    [DllImport(Lib)] public static extern void tts_free_bytes(IntPtr bytes, UIntPtr len);

    [DllImport(Lib)] public static extern void tts_set_voice(IntPtr ctx, string voiceId);
    [DllImport(Lib)] public static extern void tts_set_rate(IntPtr ctx, float rate);
    [DllImport(Lib)] public static extern void tts_set_pitch(IntPtr ctx, float pitch);
    [DllImport(Lib)] public static extern void tts_set_volume(IntPtr ctx, float volume);

    // Audio callback: cb(bytes.ptr, bytes.len, userdata). The delegate type
    // must match `extern "C" fn(*const u8, usize, *mut c_void)` on the Rust side.
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate void AudioCallbackNative(IntPtr bytes, UIntPtr len, IntPtr userdata);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate void BoundaryCallbackNative(IntPtr word, float start, float end, IntPtr userdata);
    [DllImport(Lib)] public static extern void tts_set_on_audio(IntPtr ctx, AudioCallbackNative? cb, IntPtr userdata);
    [DllImport(Lib)] public static extern void tts_set_on_boundary(IntPtr ctx, BoundaryCallbackNative? cb, IntPtr userdata);

    [DllImport(Lib)] public static extern int tts_get_voices(IntPtr ctx, out IntPtr voices, out int count);
    [DllImport(Lib)] public static extern void tts_free_voices(IntPtr voices, int count);

    [DllImport(Lib)] public static extern int tts_get_engine_count();
    [DllImport(Lib)] public static extern int tts_get_engines(out IntPtr engines, out int count);
    [DllImport(Lib)] public static extern void tts_free_engines(IntPtr engines, int count);

    [DllImport(Lib)] public static extern IntPtr tts_get_last_error(IntPtr ctx);
}

/// <summary>
/// Marshalled view of the C `tts_voice` struct. Field order MUST match the
/// Rust definition in src/types.rs (cbindgen emits the header in declaration
/// order).
/// </summary>
[StructLayout(LayoutKind.Sequential)]
public struct TtsVoiceNative
{
    public IntPtr Id;
    public IntPtr Name;
    public IntPtr Language;
    public IntPtr Gender;
    public IntPtr Engine;
}

/// <summary>
/// Marshalled view of the C `tts_engine_info` struct.
/// </summary>
[StructLayout(LayoutKind.Sequential)]
public struct TtsEngineInfoNative
{
    public IntPtr Id;
    public IntPtr Name;
    [MarshalAs(UnmanagedType.U1)] public bool NeedsCredentials;
    public IntPtr CredentialKeysJson;
}

/// <summary>
/// High-level voice descriptor returned by <see cref="TtsClient.GetVoices"/>.
/// </summary>
public sealed class TtsVoice
{
    public string Id { get; }
    public string Name { get; }
    public string Language { get; }
    public string Gender { get; }
    public string Engine { get; }

    public TtsVoice(string id, string name, string language, string gender, string engine)
    {
        Id = id; Name = name; Language = language; Gender = gender; Engine = engine;
    }

    public override string ToString() => $"{Name} ({Id}) [{Engine}/{Language}]";
}

/// <summary>
/// High-level engine descriptor returned by <see cref="TtsClient.ListEngines"/>.
/// </summary>
public sealed class TtsEngineInfo
{
    public string Id { get; }
    public string Name { get; }
    public bool NeedsCredentials { get; }
    /// <summary>JSON-encoded list of credential keys (e.g. <c>["apiKey"]</c>).</summary>
    public string CredentialKeysJson { get; }

    public TtsEngineInfo(string id, string name, bool needsCredentials, string credentialKeysJson)
    {
        Id = id; Name = name; NeedsCredentials = needsCredentials; CredentialKeysJson = credentialKeysJson;
    }

    /// <summary>Parsed credential keys (empty list if the JSON is invalid).</summary>
    public List<string> CredentialKeys
    {
        get
        {
            try
            {
                return JsonSerializer.Deserialize<List<string>>(CredentialKeysJson ?? "[]") ?? new();
            }
            catch
            {
                return new();
            }
        }
    }

    public override string ToString() => $"{Name} ({Id})";
}

/// <summary>Streaming audio chunk handed to <see cref="TtsClient.SetOnAudio"/>.</summary>
public delegate void AudioCallback(byte[] chunk);

/// <summary>Word-boundary event handed to <see cref="TtsClient.SetOnBoundary"/>.</summary>
public delegate void BoundaryCallback(string word, float startTime, float endTime);

/// <summary>
/// Exception thrown when a TTS operation fails. The message comes from
/// <c>tts_get_last_error</c> on the owning context.
/// </summary>
public class TtsException : Exception
{
    public TtsException(string message) : base(message) { }
}

/// <summary>
/// High-level .NET client for rust-tts-wrapper. Wraps a single engine
/// instance (a <c>tts_ctx*</c> on the C side) and exposes it as a managed,
/// IDisposable resource.
/// </summary>
public class TtsClient : IDisposable
{
    private IntPtr _ctx;
    private bool _disposed;

    // Strong refs to the native-callback delegates so the GC doesn't collect
    // them while native code still holds a function pointer.
    private Native.AudioCallbackNative? _audioNative;
    private Native.BoundaryCallbackNative? _boundaryNative;

    public TtsClient(string engineId = "system", Dictionary<string, string>? credentials = null)
    {
        var json = JsonSerializer.Serialize(credentials ?? new Dictionary<string, string>());
        _ctx = Native.tts_create(engineId, json);
        if (_ctx == IntPtr.Zero)
            throw new TtsException($"Failed to create engine '{engineId}': {GetGlobalLastError()}");
    }

    // --- synthesis -----------------------------------------------------

    /// <summary>Speak <paramref name="text"/> asynchronously (engine-defined).</summary>
    public void Speak(string text)
    {
        ThrowIfDisposed();
        if (Native.tts_speak(_ctx, text) != 0)
            throw new TtsException(GetLastError() ?? "unknown error");
    }

    /// <summary>Speak <paramref name="text"/> synchronously (block until done).</summary>
    public void SpeakSync(string text)
    {
        ThrowIfDisposed();
        if (Native.tts_speak_sync(_ctx, text) != 0)
            throw new TtsException(GetLastError() ?? "unknown error");
    }

    /// <summary>Synthesise <paramref name="text"/> to a PCM/MP3 byte buffer.</summary>
    public byte[] SynthToBytes(string text)
    {
        ThrowIfDisposed();
        IntPtr bufPtr;
        UIntPtr len;
        if (Native.tts_synth_to_bytes(_ctx, text, out bufPtr, out len) != 0)
            throw new TtsException(GetLastError() ?? "unknown error");

        // checked cast — UIntPtr → int can overflow on >2GiB buffers.
        long length = (long)len;
        if (length > int.MaxValue)
            throw new TtsException("Synthesised buffer exceeds 2 GiB");
        if (length == 0 || bufPtr == IntPtr.Zero) return Array.Empty<byte>();

        byte[] data = new byte[length];
        Marshal.Copy(bufPtr, data, 0, (int)length);
        Native.tts_free_bytes(bufPtr, len);
        return data;
    }

    // --- playback control ---------------------------------------------

    public void Stop() { ThrowIfDisposed(); Native.tts_stop(_ctx); }
    public void Pause() { ThrowIfDisposed(); Native.tts_pause(_ctx); }
    public void Resume() { ThrowIfDisposed(); Native.tts_resume(_ctx); }

    // --- per-instance settings ----------------------------------------

    public void SetVoice(string voiceId) { ThrowIfDisposed(); Native.tts_set_voice(_ctx, voiceId); }
    public void SetRate(float rate) { ThrowIfDisposed(); Native.tts_set_rate(_ctx, rate); }
    public void SetPitch(float pitch) { ThrowIfDisposed(); Native.tts_set_pitch(_ctx, pitch); }
    public void SetVolume(float volume) { ThrowIfDisposed(); Native.tts_set_volume(_ctx, volume); }

    // --- callbacks -----------------------------------------------------

    /// <summary>
    /// Register a streaming-audio callback. The previous callback (if any)
    /// is replaced. Pass <c>null</c> to clear.
    /// </summary>
    public void SetOnAudio(AudioCallback? callback)
    {
        ThrowIfDisposed();
        if (callback == null)
        {
            _audioNative = null;
            Native.tts_set_on_audio(_ctx, null, IntPtr.Zero);
            return;
        }

        _audioNative = (IntPtr bytes, UIntPtr len, IntPtr _userdata) =>
        {
            long n = (long)len;
            if (n == 0 || bytes == IntPtr.Zero)
            {
                callback(Array.Empty<byte>());
                return;
            }
            byte[] chunk = new byte[Math.Min(n, int.MaxValue)];
            Marshal.Copy(bytes, chunk, 0, chunk.Length);
            callback(chunk);
        };
        // Pass IntPtr.Zero as userdata — the closure captures `callback` directly.
        Native.tts_set_on_audio(_ctx, _audioNative, IntPtr.Zero);
    }

    /// <summary>
    /// Register a word-boundary callback. The previous callback (if any) is
    /// replaced. Pass <c>null</c> to clear.
    /// </summary>
    public void SetOnBoundary(BoundaryCallback? callback)
    {
        ThrowIfDisposed();
        if (callback == null)
        {
            _boundaryNative = null;
            Native.tts_set_on_boundary(_ctx, null, IntPtr.Zero);
            return;
        }

        _boundaryNative = (IntPtr wordPtr, float start, float end, IntPtr _userdata) =>
        {
            string word = wordPtr == IntPtr.Zero ? "" : Marshal.PtrToStringAnsi(wordPtr) ?? "";
            callback(word, start, end);
        };
        Native.tts_set_on_boundary(_ctx, _boundaryNative, IntPtr.Zero);
    }

    // --- enumeration ---------------------------------------------------

    /// <summary>List the voices installed on this engine.</summary>
    public List<TtsVoice> GetVoices()
    {
        ThrowIfDisposed();
        if (Native.tts_get_voices(_ctx, out IntPtr arr, out int count) != 0)
            throw new TtsException(GetLastError() ?? "unknown error");
        return MarshalVoiceArray(arr, count);
    }

    /// <summary>List all engines compiled into this build.</summary>
    public static List<TtsEngineInfo> ListEngines()
    {
        if (Native.tts_get_engines(out IntPtr arr, out int count) != 0)
            throw new TtsException(GetGlobalLastError() ?? "unknown error");
        return MarshalEngineArray(arr, count);
    }

    /// <summary>Number of engines available. Convenience over <see cref="ListEngines"/>.</summary>
    public static int EngineCount() => Native.tts_get_engine_count();

    // --- error handling ------------------------------------------------

    /// <summary>Last error for this context, or null if none.</summary>
    public string? GetLastError()
    {
        if (_ctx == IntPtr.Zero) return null;
        IntPtr ptr = Native.tts_get_last_error(_ctx);
        return ptr == IntPtr.Zero ? null : Marshal.PtrToStringAnsi(ptr);
    }

    /// <summary>Global last error (used when no context exists, e.g. tts_create).</summary>
    public static string? GetGlobalLastError()
    {
        IntPtr ptr = Native.tts_get_last_error(IntPtr.Zero);
        return ptr == IntPtr.Zero ? null : Marshal.PtrToStringAnsi(ptr);
    }

    // --- lifecycle -----------------------------------------------------

    public void Dispose()
    {
        if (_disposed) return;
        if (_ctx != IntPtr.Zero)
        {
            Native.tts_destroy(_ctx);
            _ctx = IntPtr.Zero;
        }
        _disposed = true;
        GC.SuppressFinalize(this);
    }

    ~TtsClient() => Dispose();

    // --- private helpers ----------------------------------------------

    private void ThrowIfDisposed()
    {
        if (_disposed || _ctx == IntPtr.Zero)
            throw new ObjectDisposedException(nameof(TtsClient));
    }

    private static List<TtsVoice> MarshalVoiceArray(IntPtr arr, int count)
    {
        var list = new List<TtsVoice>(count);
        if (arr == IntPtr.Zero || count <= 0) return list;

        int structSize = Marshal.SizeOf<TtsVoiceNative>();
        for (int i = 0; i < count; i++)
        {
            IntPtr item = IntPtr.Add(arr, i * structSize);
            var native = Marshal.PtrToStructure<TtsVoiceNative>(item);
            list.Add(new TtsVoice(
                PtrToUtf8(native.Id),
                PtrToUtf8(native.Name),
                PtrToUtf8(native.Language),
                PtrToUtf8(native.Gender),
                PtrToUtf8(native.Engine)));
        }
        Native.tts_free_voices(arr, count);
        return list;
    }

    private static List<TtsEngineInfo> MarshalEngineArray(IntPtr arr, int count)
    {
        var list = new List<TtsEngineInfo>(count);
        if (arr == IntPtr.Zero || count <= 0) return list;

        int structSize = Marshal.SizeOf<TtsEngineInfoNative>();
        for (int i = 0; i < count; i++)
        {
            IntPtr item = IntPtr.Add(arr, i * structSize);
            var native = Marshal.PtrToStructure<TtsEngineInfoNative>(item);
            list.Add(new TtsEngineInfo(
                PtrToUtf8(native.Id),
                PtrToUtf8(native.Name),
                native.NeedsCredentials,
                PtrToUtf8(native.CredentialKeysJson)));
        }
        Native.tts_free_engines(arr, count);
        return list;
    }

    private static string PtrToUtf8(IntPtr ptr) =>
        ptr == IntPtr.Zero ? "" : (Marshal.PtrToStringAnsi(ptr) ?? "");
}
