using System.Text.Json;

namespace RustTtsWrapper;

/// <summary>
/// Defensive JSON-field readers shared by the #102 catalogue shapes:
/// every getter is kind-guarded (an unexpected kind from a newer
/// native degrades to the default instead of throwing).
/// </summary>
internal static class JsonHelpers
{
    internal static string Str(JsonElement e, string prop) =>
        e.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() ?? "" : "";

    internal static bool Bool(JsonElement e, string prop) =>
        e.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.True && v.GetBoolean();
}

/// <summary>
/// One #102 catalogue entry (install state + unified availability).
/// <see cref="Availability"/> is one of ready / needs_download /
/// needs_bundle / needs_assets / unverified / blocked / wrong_engine;
/// <see cref="Missing"/> (needs_assets) and <see cref="Reason"/>
/// (blocked) carry the data companions.
/// </summary>
public sealed class TtsLocalModel
{
    /// <summary>The owning engine id ("sherpaonnx").</summary>
    public string Engine { get; init; } = "";

    /// <summary>The public registry id (install/uninstall/create).</summary>
    public string Id { get; init; } = "";

    /// <summary>Human-readable name.</summary>
    public string Name { get; init; } = "";

    /// <summary>Engine family ("vits", "kokoro", …).</summary>
    public string ModelType { get; init; } = "";

    /// <summary>Quality tier from the registry.</summary>
    public string Quality { get; init; } = "";

    /// <summary>(code, display name) pairs.</summary>
    public List<(string Code, string Name)> Languages { get; init; } = new();

    /// <summary>Speaker count.</summary>
    public int NumSpeakers { get; init; } = 1;

    /// <summary>Synthesis sample rate in Hz.</summary>
    public int SampleRate { get; init; }

    /// <summary>Approximate download size in MB.</summary>
    public double FileSizeMb { get; init; }

    /// <summary>License label.</summary>
    public string License { get; init; } = "";

    /// <summary>Files present in the engine's model dir.</summary>
    public bool Installed { get; init; }

    /// <summary>Installed digest ≠ registry digest (manifest-verified).</summary>
    public bool UpdateAvailable { get; init; }

    /// <summary>The engine is experimental (floravox today): badge or
    /// section these entries in UIs.</summary>
    public bool Experimental { get; init; }

    /// <summary>The unified availability value.</summary>
    public string Availability { get; init; } = "";

    /// <summary>Missing assets for needs_assets (e.g. ["vocoder"]).</summary>
    public List<string>? Missing { get; init; }

    /// <summary>Parameterized blocked reason ("no-sha256", …).</summary>
    public string? Reason { get; init; }

    internal static TtsLocalModel FromJson(JsonElement e) => new()
    {
        Engine = JsonHelpers.Str(e, "engine"),
        Id = JsonHelpers.Str(e, "id"),
        Name = JsonHelpers.Str(e, "name"),
        ModelType = JsonHelpers.Str(e, "model_type"),
        Quality = JsonHelpers.Str(e, "quality"),
        Languages = e.TryGetProperty("languages", out var langs) && langs.ValueKind == JsonValueKind.Array
            ? langs.EnumerateArray()
                .Select(l => (JsonHelpers.Str(l, "code"), JsonHelpers.Str(l, "name")))
                .ToList()
            : new(),
        NumSpeakers = e.TryGetProperty("num_speakers", out var ns) && ns.TryGetInt32(out var n) ? n : 1,
        SampleRate = e.TryGetProperty("sample_rate", out var sr) && sr.TryGetInt32(out var s) ? s : 0,
        FileSizeMb = e.TryGetProperty("filesize_mb", out var fs) && fs.TryGetDouble(out var f) ? f : 0,
        License = JsonHelpers.Str(e, "license"),
        Installed = JsonHelpers.Bool(e, "installed"),
        UpdateAvailable = JsonHelpers.Bool(e, "update_available"),
        Experimental = JsonHelpers.Bool(e, "experimental"),
        Availability = JsonHelpers.Str(e, "availability"),
        Missing = e.TryGetProperty("missing", out var miss) && miss.ValueKind == JsonValueKind.Array
            ? miss.EnumerateArray().Select(m => m.GetString() ?? "").ToList()
            : null,
        Reason = e.TryGetProperty("reason", out var r) ? r.GetString() : null,
    };

}

/// <summary>
/// One speakable installed voice from the ctx-free catalogue (#102).
/// </summary>
public sealed class TtsInstalledVoice
{
    /// <summary>The engine to create ("sherpaonnx").</summary>
    public string Engine { get; init; } = "";

    /// <summary>The model to create it for.</summary>
    public string Model { get; init; } = "";

    /// <summary>The voice id to select.</summary>
    public string Voice { get; init; } = "";

    /// <summary>Display name.</summary>
    public string Name { get; init; } = "";

    /// <summary>(bcp47, display) pairs.</summary>
    public List<(string Bcp47, string Display)> Languages { get; init; } = new();

    internal static TtsInstalledVoice FromJson(JsonElement e) => new()
    {
        Engine = JsonHelpers.Str(e, "engine"),
        Model = JsonHelpers.Str(e, "model"),
        Voice = JsonHelpers.Str(e, "voice"),
        Name = JsonHelpers.Str(e, "name"),
        Languages = e.TryGetProperty("languages", out var langs) && langs.ValueKind == JsonValueKind.Array
            ? langs.EnumerateArray()
                .Select(l => (JsonHelpers.Str(l, "bcp47"), JsonHelpers.Str(l, "display")))
                .ToList()
            : new(),
    };
}
