using System;
using System.Threading.Tasks;
using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;

namespace AvaloniaNuGetDemo;

internal static class Program
{
    [STAThread]
    public static void Main(string[] args)
    {
        // CI / smoke mode: `dotnet run -- --headless [engineId]`
        // exercises engine enumeration, client creation and voice
        // discovery without a window (used by the windows-latest
        // workflow job to verify a clean win-x64 publish loads the
        // packaged native library — GH #71 acceptance).
        if (Array.IndexOf(args, "--headless") >= 0)
        {
            string engine = ArgValue(args, "--engine") ?? "sapi";
            int rc = HeadlessSmoke.RunAsync(engine).GetAwaiter().GetResult();
            Environment.Exit(rc);
        }

        BuildAvaloniaApp().StartWithClassicDesktopLifetime(args);
    }

    private static string? ArgValue(string[] args, string name)
    {
        int i = Array.IndexOf(args, name);
        return i >= 0 && i + 1 < args.Length ? args[i + 1] : null;
    }

    public static AppBuilder BuildAvaloniaApp()
        => AppBuilder.Configure<App>()
            .UsePlatformDetect()
            .WithInterFont()
            .LogToTrace();
}
