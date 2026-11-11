using System;
using System.Collections.Generic;
using System.Text.Json;

namespace RustTtsWrapper;

/// <summary>
/// Stable error codes mirrored from the native <c>TtsErrorCode</c>
/// (GH #69). Values are part of the ABI contract — append-only.
/// </summary>
public enum TtsErrorCode : uint
{
    /// <summary>No error recorded yet.</summary>
    Unknown = 0,
    /// <summary>A required engine setting is missing (see FieldKey).</summary>
    MissingConfiguration = 1,
    /// <summary>A setting value is present but malformed.</summary>
    InvalidConfiguration = 2,
    /// <summary>The provider rejected the credentials.</summary>
    CredentialsRejected = 3,
    /// <summary>The provider could not be reached (DNS/timeout/TLS).</summary>
    NetworkError = 4,
    /// <summary>A local model or voice file is missing.</summary>
    ModelNotFound = 5,
    /// <summary>A native dependency (engine DLL/so) failed to load.</summary>
    MissingNativeDependency = 6,
    /// <summary>Unknown engine id, or compiled out of this build.</summary>
    EngineNotAvailable = 7,
    /// <summary>The engine rejected the requested language.</summary>
    LanguageNotSupported = 8,
    /// <summary>Any other engine failure.</summary>
    EngineFailure = 9,
}

/// <summary>
/// Structured error detail from <see cref="TtsClient.GetLastErrorDetail"/>
/// (GH #69): stable code, redacted engine message, safe display message,
/// engine id, and the offending credential/setup field key when known.
/// </summary>
public sealed class TtsErrorDetail
{
    /// <summary>Stable classification.</summary>
    public TtsErrorCode Code { get; init; }
    /// <summary>Wire name of the code (e.g. <c>MissingConfiguration</c>).</summary>
    public string CodeName { get; init; } = "Unknown";
    /// <summary>Engine message with obvious secrets redacted.</summary>
    public string Message { get; init; } = "";
    /// <summary>Safe, display-ready hint (never contains message content).</summary>
    public string DisplayMessage { get; init; } = "";
    /// <summary>Engine id, or empty when unknown.</summary>
    public string EngineId { get; init; } = "";
    /// <summary>Offending credential field key (e.g. <c>subscriptionKey</c>), or empty.</summary>
    public string FieldKey { get; init; } = "";

    internal static TtsErrorDetail? FromJson(string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            return new TtsErrorDetail
            {
                Code = (TtsErrorCode)root.GetProperty("code").GetUInt32(),
                CodeName = GetString(root, "name"),
                Message = GetString(root, "message"),
                DisplayMessage = GetString(root, "displayMessage"),
                EngineId = GetString(root, "engineId"),
                FieldKey = GetString(root, "fieldKey"),
            };
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static string GetString(JsonElement root, string prop) =>
        root.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.String
            ? v.GetString() ?? ""
            : "";

    public override string ToString() => $"{CodeName} [{EngineId}]" +
        (string.IsNullOrEmpty(FieldKey) ? "" : $" field:{FieldKey}") +
        $": {Message}";
}

/// <summary>
/// Outcome of a credential check with an explicit "unverified" state
/// (GH #69). Enumerating a static voice catalog succeeds without
/// authenticating, so it is not validation.
/// </summary>
public enum CredentialStatus
{
    /// <summary>The credentials were exercised and accepted.</summary>
    Verified = 1,
    /// <summary>The credentials were exercised and rejected.</summary>
    Rejected = 0,
    /// <summary>No authenticated check was possible (static catalog,
    /// no credentials, offline engine).</summary>
    Unverified = 2,
}

/// <summary>
/// Result of <see cref="TtsClient.CheckCredentials"/>: the tristate
/// status plus the error detail when the check itself failed
/// (e.g. <see cref="TtsErrorCode.MissingConfiguration"/> with the
/// offending <see cref="TtsErrorDetail.FieldKey"/>).
/// </summary>
public sealed class CredentialCheckResult
{
    /// <summary>Verified / Rejected / Unverified, or <c>null</c> when
    /// the check failed (see <see cref="Error"/>).</summary>
    public CredentialStatus? Status { get; init; }

    /// <summary>Detail for the failed check, or <c>null</c>.</summary>
    public TtsErrorDetail? Error { get; init; }

    /// <summary>True when the check ran and the credentials were accepted.</summary>
    public bool IsVerified => Status == CredentialStatus.Verified;
}

/// <summary>The kind of control a settings UI should render for a field.</summary>
public enum TtsSetupFieldKind
{
    /// <summary>Secret value — password box; never log it.</summary>
    Secret,
    /// <summary>Path to a single file — file picker.</summary>
    FilePath,
    /// <summary>Path to a directory — folder picker.</summary>
    DirectoryPath,
    /// <summary>A URL.</summary>
    Url,
    /// <summary>One of a fixed set of suggested values — combo box.</summary>
    Choice,
    /// <summary>Numeric value.</summary>
    Number,
    /// <summary>Ordinary text.</summary>
    Text,
}

/// <summary>One typed setup/credential field (GH #68).</summary>
public sealed class TtsSetupField
{
    /// <summary>Canonical credential key understood by <c>tts_create</c>.</summary>
    public string Key { get; init; } = "";
    /// <summary>Human label for the settings row.</summary>
    public string Label { get; init; } = "";
    /// <summary>Control kind.</summary>
    public TtsSetupFieldKind Kind { get; init; }
    /// <summary>Whether the engine fails without this field.</summary>
    public bool Required { get; init; }
    /// <summary>Suggested default (string form).</summary>
    public string? DefaultValue { get; init; }
    /// <summary>Suggested values for <see cref="TtsSetupFieldKind.Choice"/>.</summary>
    public IReadOnlyList<string> Choices { get; init; } = Array.Empty<string>();
    /// <summary>Placeholder text for empty inputs.</summary>
    public string? Placeholder { get; init; }
    /// <summary>One-line help text.</summary>
    public string? HelpText { get; init; }
}

/// <summary>Engine capability flags from the native registry (GH #68).</summary>
public sealed class TtsEngineCapabilities
{
    public bool SupportsRate { get; init; }
    public bool SupportsPitch { get; init; }
    public bool SupportsVolume { get; init; }
    public bool SupportsWordTiming { get; init; }
    public bool SupportsSsml { get; init; }
    public bool SupportsStreaming { get; init; }
    public bool RequiresInternet { get; init; }
    /// <summary>Voice enumeration succeeds without authentication —
    /// listing voices alone must not be read as "credentials valid".</summary>
    public bool StaticVoiceCatalog { get; init; }
    /// <summary>Backend limitations worth showing next to the controls.</summary>
    public string? Notes { get; init; }
}

/// <summary>Full setup schema for one engine (GH #68).</summary>
public sealed class TtsEngineSetup
{
    /// <summary>Engine id (matches <c>tts_create</c>).</summary>
    public string Id { get; init; } = "";
    /// <summary>Display name.</summary>
    public string Name { get; init; } = "";
    /// <summary>Whether any setup fields are required.</summary>
    public bool NeedsCredentials { get; init; }
    /// <summary>Typed setup fields (empty for zero-config engines).</summary>
    public IReadOnlyList<TtsSetupField> Fields { get; init; } = Array.Empty<TtsSetupField>();
    /// <summary>Capability flags.</summary>
    public TtsEngineCapabilities Capabilities { get; init; } = new();
    /// <summary>Either/or requirement groups: at least one field in
    /// each group must be non-empty (e.g. google: apiKey OR
    /// keyFilePath). Empty when field-level Required flags say it all.</summary>
    public IReadOnlyList<IReadOnlyList<string>> RequiredAny { get; init; } =
        Array.Empty<IReadOnlyList<string>>();

    /// <summary>
    /// Tolerant manual parse (forward compatible): an unknown field
    /// <c>kind</c> from a newer native build degrades to
    /// <see cref="TtsSetupFieldKind.Text"/> and unknown properties are
    /// ignored, instead of failing the whole
    /// <see cref="TtsClient.GetEngineSetups"/> call.
    /// </summary>
    internal static List<TtsEngineSetup>? FromJsonArray(string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var arr = doc.RootElement;
            if (arr.ValueKind != JsonValueKind.Array)
            {
                return null;
            }
            var result = new List<TtsEngineSetup>();
            foreach (var e in arr.EnumerateArray())
            {
                result.Add(new TtsEngineSetup
                {
                    Id = Str(e, "id"),
                    Name = Str(e, "name"),
                    NeedsCredentials = Bool(e, "needsCredentials"),
                    Fields = ParseFields(e),
                    Capabilities = Caps(e),
                    RequiredAny = RequiredAnyGroups(e),
                });
            }
            return result;
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static IReadOnlyList<TtsSetupField> ParseFields(JsonElement engine)
    {
        if (!engine.TryGetProperty("fields", out var arr) || arr.ValueKind != JsonValueKind.Array)
        {
            return Array.Empty<TtsSetupField>();
        }
        var list = new List<TtsSetupField>();
        foreach (var f in arr.EnumerateArray())
        {
            list.Add(new TtsSetupField
            {
                Key = Str(f, "key"),
                Label = Str(f, "label"),
                Kind = Str(f, "kind") switch
                {
                    "secret" => TtsSetupFieldKind.Secret,
                    "filePath" => TtsSetupFieldKind.FilePath,
                    "directoryPath" => TtsSetupFieldKind.DirectoryPath,
                    "url" => TtsSetupFieldKind.Url,
                    "choice" => TtsSetupFieldKind.Choice,
                    "number" => TtsSetupFieldKind.Number,
                    // Known + unknown alike fall back safely:
                    "text" or _ => TtsSetupFieldKind.Text,
                },
                Required = Bool(f, "required"),
                DefaultValue = OptStr(f, "defaultValue"),
                Choices = StrArr(f, "choices"),
                Placeholder = OptStr(f, "placeholder"),
                HelpText = OptStr(f, "helpText"),
            });
        }
        return list;
    }

    private static TtsEngineCapabilities Caps(JsonElement engine)
    {
        engine.TryGetProperty("capabilities", out var c);
        return new TtsEngineCapabilities
        {
            SupportsRate = Bool(c, "supportsRate"),
            SupportsPitch = Bool(c, "supportsPitch"),
            SupportsVolume = Bool(c, "supportsVolume"),
            SupportsWordTiming = Bool(c, "supportsWordTiming"),
            SupportsSsml = Bool(c, "supportsSsml"),
            SupportsStreaming = Bool(c, "supportsStreaming"),
            RequiresInternet = Bool(c, "requiresInternet"),
            StaticVoiceCatalog = Bool(c, "staticVoiceCatalog"),
            Notes = OptStr(c, "notes"),
        };
    }

    private static IReadOnlyList<IReadOnlyList<string>> RequiredAnyGroups(JsonElement engine)
    {
        if (!engine.TryGetProperty("requiredAny", out var arr) || arr.ValueKind != JsonValueKind.Array)
        {
            return Array.Empty<IReadOnlyList<string>>();
        }
        var groups = new List<IReadOnlyList<string>>();
        foreach (var g in arr.EnumerateArray())
        {
            groups.Add(StrArr(g, null));
        }
        return groups;
    }

    private static string Str(JsonElement e, string prop) =>
        e.ValueKind == JsonValueKind.Object
            && e.TryGetProperty(prop, out var v)
            && v.ValueKind == JsonValueKind.String
            ? v.GetString() ?? ""
            : "";

    private static string? OptStr(JsonElement e, string prop) =>
        e.ValueKind == JsonValueKind.Object && e.TryGetProperty(prop, out var v)
            ? v.GetString()
            : null;

    private static bool Bool(JsonElement e, string prop) =>
        e.ValueKind == JsonValueKind.Object
            && e.TryGetProperty(prop, out var v)
            && v.ValueKind == JsonValueKind.True;

    private static IReadOnlyList<string> StrArr(JsonElement e, string? prop)
    {
        var arr = prop is null
            ? e
            : e.ValueKind == JsonValueKind.Object && e.TryGetProperty(prop, out var v)
                ? v
                : default;
        if (arr.ValueKind != JsonValueKind.Array)
        {
            return Array.Empty<string>();
        }
        var list = new List<string>();
        foreach (var item in arr.EnumerateArray())
        {
            if (item.ValueKind == JsonValueKind.String)
            {
                list.Add(item.GetString() ?? "");
            }
        }
        return list;
    }
}
