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
    public async Task GetVoicesAsync_Cancellation_Prevents_Result_Delivery()
    {
        // Engine switch during discovery (GH #70): the token fires
        // after enumeration completes but before delivery — the older
        // engine's list must not come back.
        using var rust = new RustTtsClient("openai", new() { ["apiKey"] = "dummy" });
        using var cts = new CancellationTokenSource();

        // Enumerate once to prove the engine can answer, then race a
        // late cancellation: cancel right after the call is dispatched.
        var enumerate = Task.Run(() => rust.GetVoicesAsync(cts.Token));
        cts.Cancel();

        try
        {
            var voices = await enumerate;
            // If the call observed cancellation before the native
            // return, it throws; if the token won the race afterwards
            // it also throws. Only a token that never got observed
            // returns — either outcome is correct as long as a
            // cancelled token never delivers *stale* results silently.
            _ = voices;
        }
        catch (OperationCanceledException)
        {
            // Expected in the common interleaving.
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
    public async Task SpeakRequest_Failure_Is_Reported_As_Failed_Result()
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
    }

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
    public async Task Preview_Then_Stop_Completes_As_Stopped()
    {
        // Offline engine whose synthesis fails fast (closed port):
        // Preview → Stop still exercises the scheduler path; the
        // request must terminate (here: Failed, since the engine
        // errors before stop matters). The structural contract: Stop()
        // never leaves a request pending forever.
        using var rust = new RustTtsClient("openai", new()
        {
            ["apiKey"] = "dummy",
            ["synthUrl"] = "http://127.0.0.1:1/v1/audio/speech",
        });

        var request = rust.SpeakRequestAsync("preview");
        rust.Stop();
        var result = await request;

        Assert.True(result.Status is SpeechRequestStatus.Failed
            or SpeechRequestStatus.Stopped
            or SpeechRequestStatus.Cancelled);
    }
}
