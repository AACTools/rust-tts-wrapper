using System.Runtime.InteropServices;
using DotNetTtsWrapper.Events;
using DotNetTtsWrapper.Models;
// Both the low-level P/Invoke bindings and DotNetTtsWrapper expose a TtsVoice
// type. Alias the Rust one out of the way so the abstract-base surface wins.
using NativeVoice = RustTtsWrapper.TtsVoice;

namespace RustTtsWrapper;

/// <summary>
/// Drop-in replacement for any <see cref="AbstractTtsClient"/> backend that
/// delegates to the rust-tts-wrapper native library via the low-level
/// <see cref="TtsClient"/> P/Invoke binding.
///
/// Projects that already target <c>DotNetTtsWrapper.AbstractTtsClient</c>
/// (e.g. VoiceGarden-SAPI) can swap their backend by changing one line:
///
/// <code>
/// // Before
/// AbstractTtsClient client = new SherpaOnnxTtsClient(credentials);
///
/// // After — same surface, Rust backend underneath
/// AbstractTtsClient client = new RustTtsClient("sherpaonnx", credentials);
/// </code>
/// </summary>
public class RustTtsClient : AbstractTtsClient
{
    private readonly TtsClient _inner;

    /// <summary>
    /// Engine id as understood by rust-tts-wrapper's <c>tts_create</c>
    /// (e.g. <c>"sherpaonnx"</c>, <c>"azure"</c>, <c>"sapi"</c>).
    /// </summary>
    public string EngineId { get; }

    /// <summary>
    /// Credentials JSON string that was passed to the native engine.
    /// </summary>
    public string CredentialsJson { get; }

    /// <param name="engineId">Rust engine id (sherpaonnx / azure / google / sapi / system / ...).</param>
    /// <param name="credentials">Credential map; serialised to JSON and handed to the engine.</param>
    public RustTtsClient(string engineId, Dictionary<string, string>? credentials = null)
    {
        EngineId = engineId;
        CredentialsJson = System.Text.Json.JsonSerializer.Serialize(credentials ?? new());
        _inner = new TtsClient(engineId, credentials);

        // The Rust surface supports SSML and SpeechMarkdown directly for
        // cloud / sapi / avsynth engines; report that to callers.
        bool supportsSsml = engineId is "azure" or "google" or "sapi" or "avsynth"
            or "watson" or "polly" or "elevenlabs" or "openai";
        Capabilities = new EngineCapabilities
        {
            SupportsStreaming = true,
            SupportsWordTimings = engineId is "azure" or "google" or "sapi" or "elevenlabs",
            SupportsSsml = supportsSsml,
            SupportsSpeechMarkdown = supportsSsml,
            RequiresInternet = engineId is not "sherpaonnx" and not "sapi" and not "avsynth" and not "system",
            IsWindowsSupported = true,
            IsLinuxSupported = engineId is not "sapi" and not "avsynth",
            IsMacOsSupported = engineId is not "sapi",
        };
    }

    // -------------------------------------------------------------------
    // Voice enumeration
    // -------------------------------------------------------------------

    public override Task<List<DotNetTtsWrapper.Models.TtsVoice>> GetVoicesAsync()
    {
        // The C ABI returns Voice records synchronously; wrap as
        // Task.CompletedTask so callers can await as usual.
        var voices = _inner.GetVoices().Select(MapVoice).ToList();
        return Task.FromResult(voices);
    }

    public override Task<List<DotNetTtsWrapper.Models.TtsVoice>> GetVoicesByLanguageAsync(string languageCode)
    {
        var all = _inner.GetVoices();
        var filtered = all
            .Where(v => v.Language.StartsWith(languageCode, StringComparison.OrdinalIgnoreCase))
            .Select(MapVoice)
            .ToList();
        return Task.FromResult(filtered);
    }

    public override void SetVoice(string voiceId)
    {
        base.SetVoice(voiceId);
        _inner.SetVoice(voiceId);
    }

    // -------------------------------------------------------------------
    // Synthesis
    // -------------------------------------------------------------------

    public override async Task<TtsSynthesisResult> SynthToBytesAsync(string text, TtsOptions? options = null)
    {
        var prepared = await PrepareTextAsync(text, options);
        ApplyOptions(options);
        var audio = _inner.SynthToBytes(prepared);

        return new TtsSynthesisResult
        {
            AudioData = audio,
            Format = AudioFormat.Wav,
            SampleRate = SampleRate,
            Channels = 1,
        };
    }

    public override async Task<StreamingTtsResult> SynthToStreamAsync(string text, TtsOptions? options = null)
    {
        // The Rust C ABI doesn't expose chunked streaming for the generic
        // path; we synthesise the whole buffer then yield it as a single
        // chunk. Engines that stream natively (Azure WS) will still produce
        // audio more quickly via the underlying callbacks, but we expose
        // the final buffer here for compatibility.
        var bytes = await SynthToBytesAsync(text, options);

        var result = new StreamingTtsResult
        {
            FinalAudioData = bytes.AudioData,
            Format = AudioFormat.Wav,
            SampleRate = SampleRate,
            Channels = 1,
        };
        return result;
    }

    public override async Task SynthToFileAsync(string text, string outputPath, AudioFormat format = AudioFormat.Wav, TtsOptions? options = null)
    {
        var synth = await SynthToBytesAsync(text, options);
        await System.IO.File.WriteAllBytesAsync(outputPath, synth.AudioData);
    }

    public override async Task SpeakAsync(string text, TtsOptions? options = null)
    {
        var prepared = await PrepareTextAsync(text, options);
        ApplyOptions(options);
        // SpeakSync blocks until the engine finishes — wrap in Task.Run so
        // we don't tie up the thread pool's sync slot for long.
        await Task.Run(() => _inner.SpeakSync(prepared));
    }

    public override async Task SpeakStreamedAsync(string text, Action<WordTimingEventArgs>? wordCallback = null, TtsOptions? options = null)
    {
        if (wordCallback != null)
        {
            _inner.SetOnBoundary((word, start, end) =>
                wordCallback(new WordTimingEventArgs(word, start, end)));
        }

        try
        {
            await SpeakAsync(text, options);
        }
        finally
        {
            if (wordCallback != null) _inner.SetOnBoundary(null);
        }
    }

    // -------------------------------------------------------------------
    // Lifecycle / control
    // -------------------------------------------------------------------

    public override void Pause() => _inner.Pause();
    public override void Resume() => _inner.Resume();
    public override void Stop() => _inner.Stop();

    public override async Task<CredentialsValidationResult> CheckCredentialsAsync()
    {
        try
        {
            var voices = await GetVoicesAsync();
            return new CredentialsValidationResult
            {
                IsValid = true,
                AvailableVoiceCount = voices.Count,
                EngineName = EngineId,
            };
        }
        catch (Exception ex)
        {
            return new CredentialsValidationResult
            {
                IsValid = false,
                ErrorMessage = ex.Message,
                EngineName = EngineId,
            };
        }
    }

    public override string GetSpeechMarkdownPlatform()
    {
        // Match the DotNetTtsWrapper convention so SpeechMarkdown conversions
        // land on the same SSML dialect the Rust engine expects.
        return EngineId switch
        {
            "azure" => global::SpeechMarkdown.Platform.MicrosoftAzure,
            "google" => global::SpeechMarkdown.Platform.GoogleAssistant,
            _ => global::SpeechMarkdown.Platform.AmazonAlexa,
        };
    }

    protected override void Dispose(bool disposing)
    {
        if (disposing) _inner.Dispose();
        base.Dispose(disposing);
    }

    // -------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------

    private void ApplyOptions(TtsOptions? options)
    {
        if (options == null) return;

        if (options.VoiceId != null) _inner.SetVoice(options.VoiceId);
        if (options.Rate.HasValue) _inner.SetRate(RateToFloat(options.Rate.Value));
        if (options.Pitch.HasValue) _inner.SetPitch(PitchToFloat(options.Pitch.Value));
        if (options.Volume.HasValue) _inner.SetVolume(VolumeToFloat(options.Volume.Value));
    }

    /// <summary>DotNetTtsWrapper uses an enum (XSlow..XFast); Rust uses a float multiplier.</summary>
    private static float RateToFloat(SpeechRate rate) => rate switch
    {
        SpeechRate.XSlow => 0.5f,
        SpeechRate.Slow => 0.75f,
        SpeechRate.Medium => 1.0f,
        SpeechRate.Fast => 1.5f,
        SpeechRate.XFast => 2.0f,
        _ => 1.0f,
    };

    private static float PitchToFloat(SpeechPitch pitch) => pitch switch
    {
        SpeechPitch.XLow => 0.5f,
        SpeechPitch.Low => 0.85f,
        SpeechPitch.Medium => 1.0f,
        SpeechPitch.High => 1.4f,
        SpeechPitch.XHigh => 2.0f,
        _ => 1.0f,
    };

    /// <summary>DotNetTtsWrapper uses 0–100; Rust uses 0.0–2.0 (1.0 = normal).</summary>
    private static float VolumeToFloat(int volume) => Math.Clamp(volume, 0, 100) / 50.0f;

    private static DotNetTtsWrapper.Models.TtsVoice MapVoice(NativeVoice native)
    {
        // native.Language is BCP47-ish; map into LanguageCodes list so
        // consumers using DotNetTtsWrapper's LanguageInfo type are happy.
        var lang = native.Language;
        return new DotNetTtsWrapper.Models.TtsVoice
        {
            Id = native.Id,
            Name = native.Name,
            Provider = native.Engine,
            Gender = ParseGender(native.Gender),
            LanguageCodes = new()
            {
                new()
                {
                    Bcp47 = lang,
                    Iso639_3 = lang.Length >= 2 ? lang.Split('-')[0] : lang,
                    Display = lang,
                },
            },
        };
    }

    private static VoiceGender ParseGender(string gender)
    {
        if (string.IsNullOrEmpty(gender)) return VoiceGender.Unknown;
        return gender.ToLowerInvariant() switch
        {
            "male" => VoiceGender.Male,
            "female" => VoiceGender.Female,
            "nonbinary" or "non-binary" => VoiceGender.NonBinary,
            _ => VoiceGender.Unknown,
        };
    }
}
