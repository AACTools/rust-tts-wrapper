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

    internal static readonly JsonSerializerOptions Options = new()
    {
        PropertyNameCaseInsensitive = true,
        Converters = { new System.Text.Json.Serialization.JsonStringEnumConverter() },
    };

    internal static List<TtsEngineSetup>? FromJsonArray(string json)
    {
        try
        {
            return JsonSerializer.Deserialize<List<TtsEngineSetup>>(json, Options);
        }
        catch (JsonException)
        {
            return null;
        }
    }
}
