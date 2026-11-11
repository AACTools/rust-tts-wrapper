using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using RustTtsWrapper;

namespace AvaloniaNuGetDemo;

/// <summary>
/// Headless verification path (GH #71): proves the published app loads
/// the packaged native library with no manual DLL copy, can create a
/// client, and can enumerate voices for an offline engine. Speaks a
/// short preview through SynthToBytesAsync when a voice is installed —
/// and reports (without failing) when no voice is available.
/// </summary>
internal static class HeadlessSmoke
{
    public static async Task<int> RunAsync(string engineId)
    {
        Console.WriteLine($"[headless] engine={engineId}");

        var engines = TtsClient.ListEngines();
        Console.WriteLine($"[headless] {engines.Count} engines compiled in: " +
            $"{string.Join(", ", engines.Select(e => e.Id))}");

        var engine = engines.FirstOrDefault(e => e.Id == engineId);
        if (engine is null)
        {
            // Expected on OSes without the engine (e.g. sapi on Linux):
            // report, don't crash — the native library still loaded.
            Console.WriteLine($"[headless] engine '{engineId}' not present in this build " +
                "(feature/platform gated). Native library load itself verified.");
            return 0;
        }

        // NOTE: not `using` — disposed defensively in the finally below
        // (released-package dispose quirk, see there).
        var client = new RustTtsClient(engineId, CredentialsFor(engineId));
        Console.WriteLine($"[headless] client created for '{engineId}'");

        var voices = await client.GetVoicesAsync();
        Console.WriteLine($"[headless] {voices.Count} voices enumerated");
        foreach (var v in voices.Take(5))
        {
            Console.WriteLine($"[headless]   {v.Name} ({v.Id})");
        }

        if (voices.Count == 0)
        {
            // Documented expectation (GH #71): a settings screen shows
            // an empty voice list and a "no voices installed" hint.
            Console.WriteLine("[headless] no voices installed — expected state, " +
                "UI shows an empty list with guidance");
            return 0;
        }

        // Preview: synthesize a short utterance (no audio device
        // needed — bytes only). With a voice installed the voice IS the
        // asset, so a preview failure here is a real regression — fail
        // the smoke run rather than silently passing (GH #71).
        client.SetVoice(voices[0].Id);
        try
        {
            var audio = await client.SynthToBytesAsync("Avalonia preview");
            if (audio.AudioData.Length == 0)
            {
                Console.WriteLine("[headless] FAIL: preview synthesized 0 bytes " +
                    "with a voice installed");
                return 1;
            }
            Console.WriteLine($"[headless] preview synthesized: {audio.AudioData.Length} bytes");
            return 0;
        }
        catch (Exception ex)
        {
            Console.WriteLine($"[headless] FAIL: preview synthesis threw: {ex.Message}");
            return 1;
        }
        finally
        {
            // The RELEASED package (1.6.6) has a dispose-ordering quirk
            // (base Dispose → Stop() on the already-disposed native
            // client throws ObjectDisposedException); dispose
            // defensively until a fixed package ships.
            try { client.Dispose(); }
            catch (ObjectDisposedException) { }
        }
    }

    /// <summary>Credential keys per engine, as the Rust factory expects
    /// (canonical keys — see the migration table in the README).</summary>
    private static Dictionary<string, string>? CredentialsFor(string engineId) => engineId switch
    {
        "openai" => new() { ["apiKey"] = "dummy-key" },
        _ => null,
    };
}
