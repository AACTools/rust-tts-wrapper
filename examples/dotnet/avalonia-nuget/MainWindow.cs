using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Avalonia.Controls;
using Avalonia.Interactivity;
using Avalonia.Layout;
using RustTtsWrapper;

namespace AvaloniaNuGetDemo;

/// <summary>
/// Minimal NuGet → Avalonia demo (GH #71): engine selection, credential
/// entry, voice discovery, preview with word timings, stop — the
/// AbstractTtsClient migration target for Dasher-style desktop apps.
/// </summary>
public partial class MainWindow : Window
{
    private readonly ComboBox _engineBox = new() { Width = 260 };
    private readonly Panel _credentialPanel = new();
    private readonly ListBox _voiceList = new();
    private readonly TextBox _status = new() { IsReadOnly = true, MinHeight = 84, TextWrapping = Avalonia.Media.TextWrapping.Wrap };
    private readonly TextBox _text = new() { Text = "Hello from Rust TTS via Avalonia." };
    private readonly ObservableCollection<string> _timings = new();
    private readonly ListBox _timingList;

    private RustTtsClient? _client;
    private readonly Dictionary<string, string> _credentials = new();
    private CancellationTokenSource? _discoveryCts;

    public MainWindow()
    {
        Title = "RustTtsWrapper.Bindings — Avalonia demo";
        Width = 760;
        Height = 560;

        _timingList = new ListBox { ItemsSource = _timings, FontFamily = new Avalonia.Media.FontFamily("monospace") };

        var refreshButton = new Button { Content = "Reload voices" };
        refreshButton.Click += (_, _) => _ = ReloadVoicesAsync();

        var previewButton = new Button { Content = "Preview" };
        previewButton.Click += (_, _) => _ = PreviewAsync();

        var stopButton = new Button { Content = "Stop" };
        stopButton.Click += (_, _) => StopSpeech();

        var refresh = new Button { Content = "Engines" };
        refresh.Click += (_, _) => LoadEngines();

        var root = new StackPanel { Margin = new Avalonia.Thickness(12), Spacing = 8 };
        root.Children.Add(new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            Children = { new TextBlock { Text = "Engine:", VerticalAlignment = VerticalAlignment.Center }, _engineBox, refresh },
        });
        root.Children.Add(new TextBlock { Text = "Credentials / setup" });
        root.Children.Add(_credentialPanel);
        root.Children.Add(refreshButton);
        root.Children.Add(_voiceList);
        root.Children.Add(_text);
        root.Children.Add(new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { previewButton, stopButton } });
        root.Children.Add(new TextBlock { Text = "Word timings (request-scoped events)" });
        root.Children.Add(_timingList);
        root.Children.Add(new TextBlock { Text = "Status" });
        root.Children.Add(_status);

        Content = root;
        _engineBox.SelectionChanged += (_, _) => _ = SelectEngineAsync();

        Opened += (_, _) => LoadEngines();
        Closing += (_, _) => Cleanup();
    }

    // ── engines + credential fields ────────────────────────────────

    private void LoadEngines()
    {
        try
        {
            var ids = TtsClient.ListEngines().Select(e => e.Id).ToList();
            _engineBox.ItemsSource = ids;
            if (ids.Count > 0)
            {
                // Prefer an offline engine present in this build so the
                // demo works without credentials.
                _engineBox.SelectedItem = ids.FirstOrDefault(IsOffline) ?? ids[0];
            }
            SetStatus("Engines loaded.");
        }
        catch (Exception ex)
        {
            // Broad catch: fire-and-forget handlers swallow anything
            // unobserved in .NET Core (a missing native library would
            // otherwise vanish without a status message).
            SetStatus($"Engine enumeration failed: {ex.Message}");
        }
    }

    /// <summary>Engines that need no credentials at all (a model-free
    /// start): the demo selects one of these by default. Model-based
    /// local engines (sherpaonnx, qwen3-local, omnivoice-local, pocket)
    /// still need paths filled in before a client can be created.</summary>
    private static bool IsOffline(string id) =>
        id is "sapi" or "system" or "avsynth";

    private async Task SelectEngineAsync()
    {
        if (_engineBox.SelectedItem is not string engineId)
        {
            return;
        }

        // Engine switch during discovery (GH #70): cancel the previous
        // engine's in-flight enumeration so its results can never
        // overwrite the new engine's list.
        _discoveryCts?.Cancel();
        _discoveryCts = new CancellationTokenSource();
        _credentials.Clear();
        _client?.Dispose();
        _client = null;
        _timings.Clear();
        BuildCredentialFields(engineId);

        // Create the client immediately (offline engines work with no
        // credentials) and discover voices under the fresh token.
        try
        {
            _client = new RustTtsClient(engineId, _credentials);
            await ReloadVoicesAsync(_discoveryCts.Token);
        }
        catch (TtsException ex)
        {
            _client = null;
            SetStatus($"Client creation failed: {Describe(ex)} — fill in the fields and press Reload voices.");
        }
    }

    /// <summary>
    /// Render the credential rows for the selected engine. With local
    /// bindings this is schema-driven (typed kinds → password box, file
    /// picker, folder picker, combo — GH #68); on the released package
    /// it falls back to the engine's credential keys.
    /// </summary>
    private void BuildCredentialFields(string engineId)
    {
        _credentialPanel.Children.Clear();

#if USE_LOCAL_BINDINGS
        var setup = TtsClient.GetEngineSetup(engineId);
        if (setup is not null)
        {
            if (setup.Fields.Count == 0)
            {
                _credentialPanel.Children.Add(new TextBlock { Text = "No setup required." });
            }
            foreach (var f in setup.Fields)
            {
                _credentialPanel.Children.Add(MakeFieldRow(
                    f.Label,
                    f.Kind is TtsSetupFieldKind.Secret,
                    f.Placeholder ?? (f.Choices.Count > 0 ? f.Choices[0] : f.DefaultValue),
                    text => _credentials[f.Key] = text));
            }
            if (setup.Capabilities.Notes is { } notes)
            {
                _credentialPanel.Children.Add(new TextBlock { Text = $"ℹ {notes}", TextWrapping = Avalonia.Media.TextWrapping.Wrap });
            }
            return;
        }
#endif
        // Released-package fallback: keys only (no kinds available
        // there — the reason GH #68 exists).
        var info = TtsClient.ListEngines().FirstOrDefault(e => e.Id == engineId);
        if (info is null || info.CredentialKeys.Count == 0)
        {
            _credentialPanel.Children.Add(new TextBlock { Text = "No credentials required." });
            return;
        }
        foreach (var key in info.CredentialKeys)
        {
            bool secret = key.Contains("key", StringComparison.OrdinalIgnoreCase)
                && !key.Contains("path", StringComparison.OrdinalIgnoreCase);
            _credentialPanel.Children.Add(MakeFieldRow(key, secret, null, text => _credentials[key] = text));
        }
    }

    private static Control MakeFieldRow(string label, bool secret, string? placeholder, Action<string> onChange)
    {
        var box = new TextBox { Watermark = placeholder ?? label };
        if (secret)
        {
            // Password-style entry for secrets; visible elsewhere.
            box.PasswordChar = '•';
        }
        box.TextChanged += (_, _) => onChange(box.Text ?? "");
        return new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            Margin = new Avalonia.Thickness(0, 2),
            Children =
            {
                new TextBlock { Text = label + ":", Width = 160, VerticalAlignment = VerticalAlignment.Center },
                box,
            },
        };
    }

    // ── voice discovery (cancellable — GH #70) ─────────────────────

    private async Task ReloadVoicesAsync(CancellationToken ct = default)
    {
        if (_client is null)
        {
            return;
        }
        ct = ct == default ? (_discoveryCts?.Token ?? CancellationToken.None) : ct;
        try
        {
#if USE_LOCAL_BINDINGS
            var voices = await _client.GetVoicesAsync(ct);
#else
            // Released package: no cancellable overload yet.
            var voices = await _client.GetVoicesAsync();
#endif
            _voiceList.ItemsSource = voices.Select(v => $"{v.Name} ({v.Id})").ToList();
            SetStatus($"Loaded {voices.Count} voices." +
                (voices.Count == 0 ? " No voices installed — install one to preview." : ""));
        }
        catch (OperationCanceledException)
        {
            SetStatus("Voice discovery cancelled (engine switch).");
        }
        catch (TtsException ex)
        {
            SetStatus($"Voice discovery failed: {Describe(ex)}");
        }
    }

    private async Task ReloadVoicesAsync()
    {
        // Recreate the client with current credentials, then reload.
        // NOTE: pass the token explicitly — calling the parameterless
        // overload from here would recurse into itself (C# prefers the
        // exact-arity candidate over the optional-parameter one).
        if (_engineBox.SelectedItem is not string engineId)
        {
            return;
        }
        try
        {
            _client?.Dispose();
            _client = new RustTtsClient(engineId, _credentials);
            await ReloadVoicesAsync(_discoveryCts?.Token ?? CancellationToken.None);
        }
        catch (TtsException ex)
        {
            // With local bindings the error carries a stable code + the
            // offending field key (GH #69) — enough to highlight the
            // exact settings row.
            SetStatus($"Client creation failed: {Describe(ex)}");
        }
    }

    /// <summary>Actionable error text: stable code + field key when the
    /// bindings carry them (GH #69), plain message otherwise.</summary>
    private static string Describe(TtsException ex)
    {
#if USE_LOCAL_BINDINGS
        return $"[{ex.ErrorCode}]" +
            (string.IsNullOrEmpty(ex.FieldKey) ? "" : $" field: {ex.FieldKey}") +
            $": {ex.DisplayMessage ?? ex.Message}";
#else
        return ex.Message;
#endif
    }

    // ── preview + stop (request-scoped speech — GH #70) ────────────

    private async Task PreviewAsync()
    {
        if (_client is null)
        {
            SetStatus("Create a client first.");
            return;
        }
        _timings.Clear();
        try
        {
            // Preview the SELECTED voice (settings-screen parity with
            // the headless smoke, which also SetVoices first).
            if (_voiceList.SelectedIndex >= 0 &&
                _voiceList.SelectedItem is string entry)
            {
                var voiceId = entry.Substring(entry.LastIndexOf("(", StringComparison.Ordinal) + 1).TrimEnd(')');
                _client.SetVoice(voiceId);
            }
#if USE_LOCAL_BINDINGS
            // Replace dispatch: a new preview supersedes the previous
            // one (settings-screen preview semantics — Dasher parity).
            var result = await _client.SpeakRequestAsync(
                _text.Text ?? "",
                timing: e => _timings.Add(
                    $"{e.RequestId[..8]} {e.Word} @ {e.StartTime:0.00}s–{e.EndTime:0.00}s{(e.Estimated ? " (est.)" : "")}"),
                request: new SpeechRequestOptions { Dispatch = SpeechRequestDispatch.Replace });
            SetStatus($"Request {result.RequestId[..8]}: {result.Status}");
#else
            // Released package: legacy streamed preview.
            await _client.SpeakStreamedAsync(
                _text.Text ?? "",
                e => _timings.Add($"{e.Text} @ {e.StartTime:0.00}s–{e.EndTime:0.00}s"));
            SetStatus("Preview finished.");
#endif
        }
        catch (TtsException ex)
        {
            SetStatus($"Preview failed: {Describe(ex)}");
        }
    }

    private void StopSpeech()
    {
        // Stop cancels the active request and everything queued (GH #70).
        _client?.Stop();
        SetStatus("Stopped.");
    }

    private void SetStatus(string text) => _status.Text = $"{DateTime.Now:HH:mm:ss}  {text}";

    private void Cleanup()
    {
        _discoveryCts?.Cancel();
        _discoveryCts?.Dispose();
        _client?.Dispose();
    }
}
