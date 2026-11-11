using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;
using DotNetTtsWrapper.Events;
using DotNetTtsWrapper.Models;
// Both the low-level P/Invoke bindings and DotNetTtsWrapper expose a TtsVoice
// type. Alias the Rust one out of the way so the abstract-base surface wins.
using NativeVoice = RustTtsWrapper.TtsVoice;

namespace RustTtsWrapper;

/// <summary>
/// Detailed credential outcome (GH #69) — the tristate the
/// <see cref="RustTtsClient.CheckCredentialsDetailedAsync"/> API reports.
/// </summary>
public enum TtsCredentialStatus
{
    /// <summary>An authenticated check (or validated local setup) succeeded.</summary>
    Verified,
    /// <summary>The provider refused the credentials.</summary>
    Rejected,
    /// <summary>No authenticated check was possible (static catalog,
    /// zero-credential engine). Enumerating voices alone is not
    /// validation.</summary>
    Unverified,
    /// <summary>The check itself failed (missing field, network) — see
    /// <see cref="TtsCredentialCheckDetail.Error"/>.</summary>
    CheckFailed,
}

/// <summary>The richer credential result for settings screens (GH #69).</summary>
public sealed class TtsCredentialCheckDetail
{
    public required TtsCredentialStatus Status { get; init; }
    /// <summary>Stable error code name when <see cref="TtsCredentialStatus.CheckFailed"/>.</summary>
    public TtsErrorCode? ErrorCode { get; init; }
    /// <summary>Offending credential field key (e.g. <c>subscriptionKey</c>).</summary>
    public string? FieldKey { get; init; }
    /// <summary>Safe, display-ready message.</summary>
    public string? DisplayMessage { get; init; }
    /// <summary>Voice count when enumeration was reached.</summary>
    public int? AvailableVoiceCount { get; init; }
}

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
///
/// Beyond the abstract surface, this class exposes the native extras:
/// cancellable voice discovery (GH #70), request-scoped speech with
/// queue/replace semantics (GH #70), detailed credential checks (GH #69),
/// and the typed engine setup schema (GH #68) — see
/// <see cref="GetVoicesAsync(CancellationToken)"/>,
/// <see cref="SpeakRequestAsync"/>,
/// <see cref="CheckCredentialsDetailedAsync"/> and
/// <see cref="EngineSetup"/>.
/// </summary>
public class RustTtsClient : AbstractTtsClient
{
    private readonly TtsClient _inner;

    /// <summary>Request scheduler owning queue/replace/stop semantics
    /// and boundary-callback isolation (GH #70).</summary>
    private readonly SpeechRequestScheduler _scheduler;

    /// <summary>
    /// Engine id as understood by rust-tts-wrapper's <c>tts_create</c>
    /// (e.g. <c>"sherpaonnx"</c>, <c>"azure"</c>, <c>"sapi"</c>).
    /// </summary>
    public string EngineId { get; }

    /// <summary>
    /// Credentials JSON string that was passed to the native engine.
    /// </summary>
    public string CredentialsJson { get; }

    /// <summary>
    /// The typed setup schema for this engine (GH #68): credential
    /// fields with kinds/labels/defaults plus capability flags. Null on
    /// native builds without the schema API (or an unknown engine id).
    /// </summary>
    public TtsEngineSetup? EngineSetup { get; }

    /// <param name="engineId">Rust engine id (sherpaonnx / azure / google / sapi / system / ...).</param>
    /// <param name="credentials">Credential map; serialised to JSON and handed to the engine.</param>
    public RustTtsClient(string engineId, Dictionary<string, string>? credentials = null)
    {
        EngineId = engineId;
        CredentialsJson = System.Text.Json.JsonSerializer.Serialize(credentials ?? new());
        _inner = new TtsClient(engineId, credentials);
        _scheduler = new SpeechRequestScheduler(new TtsClientBackend(_inner));

        // GH #68: capabilities come from the native registry when
        // available (single source of truth, includes local engines);
        // the legacy hard-coded table remains as the fallback for older
        // native builds.
        EngineSetup = TryLoadEngineSetup(engineId);
        Capabilities = EngineSetup is { } setup
            ? FromNativeCapabilities(setup)
            : LegacyCapabilities(engineId);
    }

    private static TtsEngineSetup? TryLoadEngineSetup(string engineId)
    {
        try
        {
            return TtsClient.GetEngineSetup(engineId);
        }
        catch (Exception ex) when (ex is TtsException or DllNotFoundException or EntryPointNotFoundException
            or MissingMethodException or System.Runtime.InteropServices.MarshalDirectiveException)
        {
            // Older native build without tts_get_engine_setups_json.
            return null;
        }
    }

    private static EngineCapabilities FromNativeCapabilities(TtsEngineSetup setup) =>
        new()
        {
            SupportsStreaming = setup.Capabilities.SupportsStreaming,
            SupportsWordTimings = setup.Capabilities.SupportsWordTiming,
            SupportsSsml = setup.Capabilities.SupportsSsml,
            SupportsSpeechMarkdown = setup.Capabilities.SupportsSsml,
            RequiresInternet = setup.Capabilities.RequiresInternet,
            IsWindowsSupported = true,
            IsLinuxSupported = engineNotMacOnly(setup.Id),
            IsMacOsSupported = engineNotWindowsOnly(setup.Id),
        };

    private static bool engineNotMacOnly(string id) => id is not "avsynth";
    private static bool engineNotWindowsOnly(string id) => id is not "sapi";

    private static EngineCapabilities LegacyCapabilities(string engineId)
    {
        // Pre-schema fallback (kept for older native builds).
        bool supportsSsml = engineId is "azure" or "google" or "sapi" or "avsynth"
            or "watson" or "polly" or "elevenlabs" or "openai";
        return new EngineCapabilities
        {
            SupportsStreaming = true,
            SupportsWordTimings = engineId is "azure" or "google" or "sapi" or "elevenlabs",
            SupportsSsml = supportsSsml,
            SupportsSpeechMarkdown = supportsSsml,
            RequiresInternet = engineId is not "sherpaonnx" and not "sapi" and not "avsynth"
                and not "system" and not "qwen3-local" and not "omnivoice-local"
                and not "floravox" and not "pocket",
            IsWindowsSupported = true,
            IsLinuxSupported = engineId is not "sapi" and not "avsynth",
            IsMacOsSupported = engineId is not "sapi",
        };
    }

    // -------------------------------------------------------------------
    // Voice enumeration
    // -------------------------------------------------------------------

    public override Task<List<DotNetTtsWrapper.Models.TtsVoice>> GetVoicesAsync()
        => GetVoicesAsync(CancellationToken.None);

    /// <summary>
    /// Cancellable voice discovery (GH #70). Enumeration runs on the
    /// thread pool so the UI never blocks, and a cancelled token stops
    /// the result from being delivered:
    /// <list type="bullet">
    /// <item>Cancelled before the native call starts → the task
    /// cancels without touching the engine.</item>
    /// <item>Cancelled while the native call is in flight → the token
    /// is observed as soon as the call returns and the task cancels;
    /// native backends cannot abort an in-flight enumeration, so the
    /// engine call itself runs to completion (documented native
    /// limitation). A cancelled discovery never publishes results, so
    /// an engine switch during discovery cannot overwrite the newer
    /// engine's voice list.</item>
    /// </list>
    /// </summary>
    public async Task<List<DotNetTtsWrapper.Models.TtsVoice>> GetVoicesAsync(CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();

        var voices = await Task.Run(() => _inner.GetVoices(), cancellationToken)
            .ConfigureAwait(false);

        // Observe late cancellation before handing results back: a
        // caller that switched engines and cancelled must not receive
        // (or act on) this engine's list.
        cancellationToken.ThrowIfCancellationRequested();

        return voices.Select(MapVoice).ToList();
    }

    public override Task<List<DotNetTtsWrapper.Models.TtsVoice>> GetVoicesByLanguageAsync(string languageCode)
        => GetVoicesByLanguageAsync(languageCode, CancellationToken.None);

    /// <summary>Cancellable variant of <see cref="GetVoicesByLanguageAsync"/> (GH #70).</summary>
    public async Task<List<DotNetTtsWrapper.Models.TtsVoice>> GetVoicesByLanguageAsync(
        string languageCode, CancellationToken cancellationToken)
    {
        var all = await GetVoicesAsync(cancellationToken).ConfigureAwait(false);
        return all
            .Where(v => v.LanguageCodes.Any(lc =>
                lc.Bcp47.StartsWith(languageCode, StringComparison.OrdinalIgnoreCase)))
            .ToList();
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
        // Queue behind anything outstanding so SpeakAsync and request-
        // scoped speech share one serialized path (GH #70).
        var result = await SpeakRequestAsync(
            prepared,
            timing: null,
            options,
            new SpeechRequestOptions(),
            CancellationToken.None).ConfigureAwait(false);
        if (result.Status == SpeechRequestStatus.Failed)
        {
            // Rethrow the original engine exception so callers keep the
            // structured TtsException detail (GH #69).
            if (result.ErrorException is TtsException or OperationCanceledException)
            {
                System.Runtime.ExceptionServices.ExceptionDispatchInfo
                    .Capture(result.ErrorException).Throw();
            }
            throw new TtsException(result.Error ?? "speech failed");
        }
    }

    /// <summary>
    /// Request-scoped speech (GH #70): a unique request id in every
    /// timing/completion event, explicit Queue or Replace semantics,
    /// cooperative cancellation, and guaranteed callback isolation —
    /// a cancelled or superseded request can never deliver callbacks
    /// to a newer request.
    ///
    /// See <see cref="SpeechRequestScheduler"/> docs for the exact
    /// queue/replace/stop contract.
    /// </summary>
    /// <param name="text">Text to speak.</param>
    /// <param name="timing">Per-request word-timing callback (carries the request id and offsets).</param>
    /// <param name="options">Rate/pitch/volume/voice options.</param>
    /// <param name="request">Queue or Replace dispatch.</param>
    /// <param name="cancellationToken">Cooperative stop while speaking.</param>
    public async Task<SpeechRequestResult> SpeakRequestAsync(
        string text,
        Action<SpeechTimingEventArgs>? timing = null,
        TtsOptions? options = null,
        SpeechRequestOptions? request = null,
        CancellationToken cancellationToken = default)
    {
        var prepared = await PrepareTextAsync(text, options).ConfigureAwait(false);
        // Options apply when the request starts (under the gate), not
        // when it is enqueued — an earlier request must not clobber a
        // later one's settings (GH #70).
        return await _scheduler.SubmitAsync(
            prepared,
            timing,
            prepare: () => ApplyOptions(options),
            request,
            cancellationToken).ConfigureAwait(false);
    }

    public override async Task SpeakStreamedAsync(string text, Action<WordTimingEventArgs>? wordCallback = null, TtsOptions? options = null)
    {
        // Legacy surface on top of the scheduler: keep the exact
        // WordTimingEventArgs shape, gain queueing + callback isolation.
        var result = await SpeakRequestAsync(
            text,
            wordCallback is null ? null : e =>
                wordCallback(new WordTimingEventArgs(e.Word, e.StartTime, e.EndTime)),
            options,
            new SpeechRequestOptions(),
            CancellationToken.None).ConfigureAwait(false);
        if (result.Status == SpeechRequestStatus.Failed)
        {
            // Rethrow the original engine exception (structured detail,
            // GH #69) — the pre-scheduler behaviour.
            if (result.ErrorException is TtsException or OperationCanceledException)
            {
                System.Runtime.ExceptionServices.ExceptionDispatchInfo
                    .Capture(result.ErrorException).Throw();
            }
            throw new TtsException(result.Error ?? "speech failed");
        }
    }

    // -------------------------------------------------------------------
    // Lifecycle / control
    // -------------------------------------------------------------------

    public override void Pause() => _inner.Pause();
    public override void Resume() => _inner.Resume();

    /// <summary>
    /// Stop the active request and cancel everything queued (GH #70):
    /// the active request completes
    /// <see cref="SpeechRequestStatus.Stopped"/>; queued requests
    /// complete <see cref="SpeechRequestStatus.Cancelled"/> and never
    /// start.
    /// </summary>
    public override void Stop() => _scheduler.StopAll();

    // -------------------------------------------------------------------
    // Credential checks (GH #69)
    // -------------------------------------------------------------------

    /// <summary>
    /// Legacy bool-shaped check: Verified → valid; Rejected → invalid;
    /// Unverified → reported valid (the engine enumerated its catalog)
    /// with a note in <see cref="CredentialsValidationResult.ErrorMessage"/>
    /// — a settings screen that needs the distinction should use
    /// <see cref="CheckCredentialsDetailedAsync"/> instead.
    /// </summary>
    public override async Task<CredentialsValidationResult> CheckCredentialsAsync()
    {
        var detail = await CheckCredentialsDetailedAsync().ConfigureAwait(false);
        return detail.Status switch
        {
            TtsCredentialStatus.Verified => new CredentialsValidationResult
            {
                IsValid = true,
                EngineName = EngineId,
            },
            TtsCredentialStatus.Unverified => new CredentialsValidationResult
            {
                IsValid = true,
                EngineName = EngineId,
                ErrorMessage = "Credentials could not be verified (static voice catalog).",
            },
            _ => new CredentialsValidationResult
            {
                IsValid = false,
                EngineName = EngineId,
                ErrorMessage = detail.DisplayMessage
                    ?? detail.ErrorCode?.ToString()
                    ?? "Credential check failed",
            },
        };
    }

    /// <summary>
    /// Tristate credential check (GH #69). Reports
    /// <see cref="TtsCredentialStatus.Verified"/>,
    /// <see cref="TtsCredentialStatus.Rejected"/> or
    /// <see cref="TtsCredentialStatus.Unverified"/> — enumerating a
    /// static voice catalog (OpenAI-style) yields Unverified, never
    /// Verified. A failed check (missing required field, unreachable
    /// provider) yields <see cref="TtsCredentialStatus.CheckFailed"/>
    /// with the stable <see cref="TtsCredentialCheckDetail.ErrorCode"/>
    /// and <see cref="TtsCredentialCheckDetail.FieldKey"/> so the UI
    /// can point at the row to fix.
    /// </summary>
    public async Task<TtsCredentialCheckDetail> CheckCredentialsDetailedAsync()
    {
        // Run the (potentially network-bound) check off the caller's
        // context, like every other engine operation here.
        var native = await Task.Run(() => _inner.CheckCredentials()).ConfigureAwait(false);

        if (native.Status is { } status)
        {
            return new TtsCredentialCheckDetail
            {
                Status = MapStatus(status),
                AvailableVoiceCount = await CountVoicesQuietly().ConfigureAwait(false),
            };
        }

        var error = native.Error;
        return new TtsCredentialCheckDetail
        {
            Status = TtsCredentialStatus.CheckFailed,
            ErrorCode = error?.Code,
            FieldKey = error?.FieldKey,
            DisplayMessage = error?.DisplayMessage.Length > 0
                ? error.DisplayMessage
                : error?.Message,
        };

        static TtsCredentialStatus MapStatus(CredentialStatus s) => s switch
        {
            CredentialStatus.Verified => TtsCredentialStatus.Verified,
            CredentialStatus.Rejected => TtsCredentialStatus.Rejected,
            _ => TtsCredentialStatus.Unverified,
        };
    }

    /// <summary>Best-effort voice count; enumeration errors read as null.</summary>
    private async Task<int?> CountVoicesQuietly()
    {
        try
        {
            var voices = await GetVoicesAsync().ConfigureAwait(false);
            return voices.Count;
        }
        catch (TtsException)
        {
            return null;
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
        if (disposing)
        {
            _scheduler.Dispose();
            _inner.Dispose();
        }
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
