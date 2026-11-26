using System;
using System.Text.Json;

namespace RustTtsWrapper;

/// <summary>
/// What one synthesis cost, from <see cref="TtsClient.SynthToBytesMeasured"/>.
/// Every engine is measured the same way, from the outside: when its first
/// audio arrived, when all of it had, and how much there was. So a local
/// model and a cloud provider can be put side by side.
/// </summary>
public sealed class SynthesisDiagnostics
{
    /// <summary>From the call until the engine returned, all the audio delivered.</summary>
    public ulong WallMs { get; init; }

    /// <summary>
    /// Until the first audio arrived: how long a listener waits. Null when no
    /// audio arrived. Well below <see cref="WallMs"/> the engine streamed, and
    /// the audio could be played as it was made; close to it, the audio only
    /// came at the end.
    /// </summary>
    public ulong? FirstAudioMs { get; init; }

    /// <summary>The audio delivered, in bytes.</summary>
    public ulong AudioBytes { get; init; }

    /// <summary>How many times audio was delivered.</summary>
    public ulong Chunks { get; init; }

    internal static SynthesisDiagnostics? FromJson(string json)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                return null;
            }
            return new SynthesisDiagnostics
            {
                WallMs = Number(root, "wallMs") ?? 0,
                FirstAudioMs = Number(root, "firstAudioMs"),
                AudioBytes = Number(root, "audioBytes") ?? 0,
                Chunks = Number(root, "chunks") ?? 0,
            };
        }
        catch (JsonException)
        {
            return null;
        }
    }

    // Null for an absent key, a null, or anything that is not a number:
    // a newer native may say "no first audio" in a way this build does
    // not know, and that must read as unknown, not as zero.
    private static ulong? Number(JsonElement root, string prop) =>
        root.TryGetProperty(prop, out var v) && v.ValueKind == JsonValueKind.Number
            ? v.GetUInt64()
            : null;

    public override string ToString() =>
        $"{WallMs} ms for {AudioBytes} bytes in {Chunks} deliveries, first audio at "
        + (FirstAudioMs is { } first ? $"{first} ms" : "never");
}

/// <summary>Audio from one synthesis, and what the synthesis cost.</summary>
public sealed class MeasuredSynthesis
{
    /// <summary>The audio; empty when the engine delivered none.</summary>
    public byte[] Audio { get; init; } = Array.Empty<byte>();

    /// <summary>What the synthesis cost.</summary>
    public SynthesisDiagnostics Diagnostics { get; init; } = new();
}
