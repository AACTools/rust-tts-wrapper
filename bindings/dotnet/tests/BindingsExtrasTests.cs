using System;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Xunit;

namespace RustTtsWrapper.Bindings.Tests;

/// <summary>
/// Native-backed tests for the GH #68/#69/#70 surface. Requires the
/// shared library (see AbiConformanceTests for the loading contract).
/// Offline-deterministic: cloud engines construct without network, and
/// a synthUrl override at a closed local port fails deterministically.
/// </summary>
public sealed class BindingsExtrasTests : IDisposable
{
    private readonly TtsClient _client =
        new("openai", new() { ["apiKey"] = "dummy-key-for-tests" });

    public void Dispose() => _client.Dispose();

    // ── GH #68: typed engine setup schema ───────────────────────────

    [Fact]
    public void EngineSetups_Render_Azure_Key_And_Region()
    {
        var setups = TtsClient.GetEngineSetups();
        Assert.NotEmpty(setups);

        var azure = setups.FirstOrDefault(s => s.Id == "azure");
        Assert.NotNull(azure);

        var key = azure!.Fields.First(f => f.Key == "subscriptionKey");
        Assert.Equal(TtsSetupFieldKind.Secret, key.Kind);
        Assert.True(key.Required);

        var region = azure.Fields.First(f => f.Key == "region");
        Assert.True(region.Required);
        Assert.NotEmpty(region.Choices);

        Assert.True(azure.Capabilities.SupportsStreaming);
        Assert.False(azure.Capabilities.StaticVoiceCatalog);
    }

    [Fact]
    public void EngineSetups_Google_Has_File_Picker_Field()
    {
        var google = TtsClient.GetEngineSetup("google");
        Assert.NotNull(google);

        var file = google!.Fields.FirstOrDefault(f => f.Key == "keyFilePath");
        Assert.NotNull(file);
        Assert.Equal(TtsSetupFieldKind.FilePath, file!.Kind);
        Assert.False(file.Required);

        var apiKey = google.Fields.First(f => f.Key == "apiKey");
        Assert.Equal(TtsSetupFieldKind.Secret, apiKey.Kind);
    }

    [Fact]
    public void EngineSetups_Cover_Every_Listed_Engine()
    {
        // The schema registry and the legacy engine list must agree —
        // a settings UI built on the schema never misses an engine.
        var listed = TtsClient.ListEngines().Select(e => e.Id).ToHashSet();
        var schema = TtsClient.GetEngineSetups().Select(s => s.Id).ToHashSet();
        Assert.True(listed.SetEquals(schema),
            $"mismatch: listed-only={string.Join(",", listed.Except(schema))} " +
            $"schema-only={string.Join(",", schema.Except(listed))}");
    }

    // ── GH #69: actionable errors ───────────────────────────────────

    [Fact]
    public void Missing_Configuration_Carries_The_Field_Key()
    {
        // Cloud engines construct lazily; the credential check names
        // the missing field with a stable code.
        using var azure = new TtsClient("azure", new Dictionary<string, string>());
        var result = azure.CheckCredentials();

        Assert.Null(result.Status);
        Assert.NotNull(result.Error);
        Assert.Equal(TtsErrorCode.MissingConfiguration, result.Error!.Code);
        Assert.Equal("subscriptionKey", result.Error.FieldKey);
        Assert.Equal("azure", result.Error.EngineId);
        Assert.Contains("setting", result.Error.DisplayMessage, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void Static_Catalog_Check_Is_Unverified_Not_Valid()
    {
        // OpenAI's voice list is static: enumeration works with any key,
        // so the check must answer Unverified — offline, deterministically.
        var result = _client.CheckCredentials();
        Assert.Equal(CredentialStatus.Unverified, result.Status);
    }

    [Fact]
    public void Synth_Failure_Yields_Stable_Code_And_Redacted_Detail()
    {
        using var unreachable = new TtsClient("openai", new()
        {
            ["apiKey"] = "sk-abcdef1234567890abcdef",
            ["synthUrl"] = "http://127.0.0.1:1/v1/audio/speech",
        });

        var ex = Assert.Throws<TtsException>(() => unreachable.SpeakSync("probe"));
        // Closed local port → connection refused: NetworkError on every
        // machine, no DNS, no timeouts.
        Assert.Equal(TtsErrorCode.NetworkError, ex.ErrorCode);

        var detail = unreachable.GetLastErrorDetail();
        Assert.NotNull(detail);
        Assert.DoesNotContain("abcdef1234567890", ex.Message);
    }

    [Fact]
    public void Create_Failure_Throws_Structured_Exception()
    {
        var ex = Assert.Throws<TtsException>(() =>
            new TtsClient("definitely-not-an-engine"));
        Assert.Equal(TtsErrorCode.EngineNotAvailable, ex.ErrorCode);
        Assert.NotNull(ex.DisplayMessage);
    }

    // ── GH #70: cancellable discovery + request-scoped speech ──────

    [Fact]
    public async Task GetVoicesAsync_Precancelled_Throws_Without_Enumerating()
    {
        using var rust = new RustTtsClient("openai", new() { ["apiKey"] = "dummy" });
        using var cts = new CancellationTokenSource();
        cts.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() =>
            rust.GetVoicesAsync(cts.Token));
    }

    [Fact]
    public async Task GetVoicesAsync_Cancellation_Mid_Flight_Never_Delivers_Results()
    {
        // Engine switch during discovery (GH #70): cancel while
        // enumeration is in flight — the contract is the task throws
        // OCE, or (when the engine finishes first) returns its own
        // complete, current list — never a stale result. Fast engines
        // (2-voice SAPI) can win the race every time; slow ones
        // (speech-dispatcher's 14k voices) land mid-flight. Both
        // outcomes are asserted honestly here.
        var engines = TtsClient.ListEngines().Select(e => e.Id).ToList();
        string engineId = engines.Contains("system")
            ? "system"
            : engines.Contains("sapi") ? "sapi" : "";
        if (engineId == "")
        {
            return; // no OS enumerator in this build; pre-cancelled test covers the rest
        }

        var rust = new RustTtsClient(engineId);
        try
        {
            // Hermeticity guard: skip when this environment cannot
            // enumerate at all (e.g. no speech-dispatcher daemon).
            try
            {
                _ = await rust.GetVoicesAsync();
            }
            catch (TtsException)
            {
                return;
            }

            bool observedMidFlight = false;
            for (int attempt = 0; attempt < 5; attempt++)
            {
                using var cts = new CancellationTokenSource();
                var enumerate = rust.GetVoicesAsync(cts.Token);
                cts.Cancel(); // immediately: maximise the mid-flight window

                try
                {
                    var voices = await enumerate;
                    // The engine won the race: the delivered list must
                    // be this engine's own complete list — never stale.
                    var fresh = await rust.GetVoicesAsync();
                    Assert.Equal(fresh.Count, voices.Count);
                }
                catch (OperationCanceledException)
                {
                    observedMidFlight = true;
                    break;
                }
            }

            if (!observedMidFlight)
            {
                Console.WriteLine(
                    "[test] engine enumerates faster than cancellation in all " +
                    "attempts; mid-flight path covered by the scheduler unit tests");
            }
        }
        finally
        {
            rust.Dispose();
        }
    }

    [Fact]
    public async Task CheckCredentialsDetailed_Reports_Missing_Field()
    {
        using var rust = new RustTtsClient("azure");
        var detail = await rust.CheckCredentialsDetailedAsync();

        Assert.Equal(TtsCredentialStatus.CheckFailed, detail.Status);
        Assert.Equal(TtsErrorCode.MissingConfiguration, detail.ErrorCode);
        Assert.Equal("subscriptionKey", detail.FieldKey);
    }

    [Fact]
    public async Task CheckCredentialsDetailed_Static_Catalog_Is_Unverified()
    {
        using var rust = new RustTtsClient("openai", new() { ["apiKey"] = "dummy" });
        var detail = await rust.CheckCredentialsDetailedAsync();
        Assert.Equal(TtsCredentialStatus.Unverified, detail.Status);
    }

    [Fact]
    public async Task Legacy_CheckCredentials_Maps_Tristate_Without_Regressing()
    {
        // Unverified keeps IsValid=true (the catalog enumerated — the
        // pre-GH #69 behaviour), with a note; the tri-state detail is
        // available via CheckCredentialsDetailedAsync.
        using var rust = new RustTtsClient("openai", new() { ["apiKey"] = "dummy" });
        var legacy = await rust.CheckCredentialsAsync();
        Assert.True(legacy.IsValid);
        Assert.NotNull(legacy.ErrorMessage);
    }

    [Fact]
    public async Task SpeakRequest_Failure_Preserves_Structured_Exception()
    {
        using var rust = new RustTtsClient("openai", new()
        {
            ["apiKey"] = "dummy",
            ["synthUrl"] = "http://127.0.0.1:1/v1/audio/speech",
        });

        var result = await rust.SpeakRequestAsync("preview text");
        Assert.Equal(SpeechRequestStatus.Failed, result.Status);
        Assert.False(string.IsNullOrEmpty(result.Error));
        Assert.False(string.IsNullOrEmpty(result.RequestId));
        // The original engine exception survives for rethrow (legacy
        // wrappers keep the structured #69 detail).
        Assert.NotNull(result.ErrorException);

        // And the legacy SpeakStreamedAsync path rethrows it with the
        // stable code intact.
        var ex = await Assert.ThrowsAsync<TtsException>(() =>
            rust.SpeakStreamedAsync("preview text"));
        Assert.Equal(TtsErrorCode.NetworkError, ex.ErrorCode);
    }

    // Preview → Preview → Stop with a real slow engine is exercised
    // against the fake backend in SpeechRequestSchedulerTests
    // (Replace_Supersedes_Active_Request, Stop_Cancels_Active_And_Queued,
    // Late_Events_After_StopAll_Are_Dropped); offline native engines
    // cannot stretch the interleaving deterministically.

    [Fact]
    public void RustTtsClient_Capabilities_Come_From_The_Schema()
    {
        using var rust = new RustTtsClient("openai", new() { ["apiKey"] = "dummy" });
        Assert.NotNull(rust.EngineSetup);
        Assert.True(rust.Capabilities.RequiresInternet);
        // OpenAI's catalog is static in the schema registry — the same
        // flag CheckCredentialsDetailedAsync leans on (GH #69).
        Assert.True(rust.EngineSetup!.Capabilities.StaticVoiceCatalog);
    }

    [Fact]
    public void Offline_Local_Engines_Do_Not_Require_Internet_In_Schema()
    {
        // The legacy capability table missed the local engines (they
        // read RequiresInternet=true); the schema registry fixes that
        // (GH #68). Only engines compiled into this build appear, so
        // assert conditionally per known local ids.
        var setups = TtsClient.GetEngineSetups();
        foreach (var id in new[] { "qwen3-local", "omnivoice-local", "floravox", "pocket", "sherpaonnx" })
        {
            var setup = setups.FirstOrDefault(s => s.Id == id);
            if (setup is null) continue; // not compiled in this build
            Assert.False(setup.Capabilities.RequiresInternet,
                $"{id} is offline — schema must not require internet");
        }
    }

    [Fact]
    public async Task Legacy_SpeakStreamed_Preview_Fails_Fast_Through_Scheduler()
    {
        // The offline preview path (closed port → instant failure) must
        // surface the structured error through the legacy wrapper.
        using var rust = new RustTtsClient("openai", new()
        {
            ["apiKey"] = "dummy",
            ["synthUrl"] = "http://127.0.0.1:1/v1/audio/speech",
        });

        var ex = await Assert.ThrowsAsync<TtsException>(() =>
            rust.SpeakStreamedAsync("preview"));
        Assert.Equal(TtsErrorCode.NetworkError, ex.ErrorCode);

        // Stop after a completed/failed request is a safe no-op.
        rust.Stop();
    }
}
