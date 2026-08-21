// ABI conformance tests for the .NET binding — mirrors bindings/c
// (the C acceptance harness) and tests/ffi_conformance.rs.
//
// The shared library must be built first:
//   cargo build --no-default-features --features system,cloud   (Linux)
// and located via TTS_WRAPPER_LIB, or on the OS loader's path.

namespace RustTtsWrapper.Bindings.Tests;

public class AbiConformanceTests
{
    private static TtsClient MakeClient() =>
        new("openai", new Dictionary<string, string>
        {
            ["apiKey"] = "dummy-key-for-dotnet-tests",
        });

    [Fact]
    public void EngineEnumerationMatchesCount()
    {
        int count = TtsClient.EngineCount();
        Assert.True(count > 0);

        var engines = TtsClient.ListEngines();
        Assert.Equal(count, engines.Count);
        Assert.All(engines, e =>
        {
            Assert.False(string.IsNullOrEmpty(e.Id));
            Assert.False(string.IsNullOrEmpty(e.Name));
        });
        Assert.Contains(engines, e => e.Id == "openai");
    }

    [Fact]
    public void CreateDisposeRoundTrip_DoubleDisposeSafe()
    {
        var c = MakeClient();
        c.Dispose();
        c.Dispose();
        Assert.Throws<ObjectDisposedException>(() => c.Speak("x"));
    }

    [Fact]
    public void CreateFailureSurfacesGlobalError()
    {
        var ex = Assert.Throws<TtsException>(() => new TtsClient("no-such-engine"));
        Assert.Contains("no-such-engine", ex.Message);
    }

    [Fact]
    public void ManyClientsLiveSimultaneously()
    {
        var clients = Enumerable.Range(0, 8).Select(_ => MakeClient()).ToList();
        Assert.All(clients, c => Assert.NotNull(c.GetVoices()));
        clients.ForEach(c => c.Dispose());
    }

    [Fact]
    public void SettersAcceptTypicalValues()
    {
        using var c = MakeClient();
        c.SetVoice("alloy");
        c.SetVoice("");
        c.SetRate(1.5f);
        c.SetPitch(0.8f);
        c.SetVolume(0.9f);
        c.Stop();
        c.Pause();
        c.Resume();
    }

    [Fact]
    public void GetVoicesReturnsArray_EmptyOfflineIsFine()
    {
        using var c = MakeClient();
        var voices = c.GetVoices();
        Assert.All(voices, v => Assert.False(string.IsNullOrEmpty(v.Id)));
    }

    [Fact]
    public void DummyKeySynthesisFailsObservably()
    {
        // Offline → validation error; online → 401. Both must surface as
        // a TtsException, never a silent success.
        using var c = MakeClient();
        Assert.Throws<TtsException>(() => c.SpeakSync("hello dotnet"));
        Assert.False(string.IsNullOrEmpty(c.GetLastError()));
        Assert.ThrowsAny<Exception>(() => c.SynthToBytes("hello dotnet"));
    }

    [Fact]
    public void CallbackRegistrationDoesNotThrow()
    {
        using var c = MakeClient();
        c.SetOnAudio(_ => { });
        c.SetOnBoundary((word, charOffset, charLen, start, end, estimated) =>
        {
            _ = (word, charOffset, charLen, start, end, estimated);
        });
        c.SetOnMark((name, charOffset, start, end) => _ = (name, charOffset, start, end));
        c.SetOnViseme((id, offsetSec) => _ = (id, offsetSec));
        c.SetOnStart(() => { });
        c.SetOnEnd(() => { });
        c.SetOnError(_ => { });

        // Clearing is a silent no-op.
        c.SetOnAudio(null);
        c.SetOnBoundary(null);
        c.SetOnMark(null);
        c.SetOnViseme(null);
        c.SetOnStart(null);
        c.SetOnEnd(null);
        c.SetOnError(null);
    }
}
