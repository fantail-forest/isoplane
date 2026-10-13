using System.Globalization;
using System.Text.Json.Nodes;
using Isoplane.Native;

namespace Isoplane.Cli;

/// <summary><c>isoplane cache info</c> and <c>isoplane cache clean</c>, over the per-user cache of compiled runtimes.</summary>
internal static class CacheCommands
{
    internal static int Run(string[] arguments, string directory, TextWriter output) => arguments switch
    {
        ["cache", "info"] => Info(directory, output),
        ["cache", "clean"] => Clean(directory, output),
        _ => throw new ArgumentException("Use isoplane cache info or isoplane cache clean."),
    };

    private static int Info(string directory, TextWriter output)
    {
        var report = RuntimeCache.Inventory(directory);
        var entries = report["entries"]!.AsArray();
        output.WriteLine($"Compiled runtime cache: {directory}");
        output.WriteLine(entries.Count == 0
            ? $"Empty. The first session compiles the runtime it needs; the cache keeps at most {Megabytes(Bytes(report["budget_bytes"]))}."
            : $"{Runtimes(entries.Count)}, {Megabytes(entries.Sum(Bytes))} of at most {Megabytes(Bytes(report["budget_bytes"]))}.");
        foreach (var entry in entries.OrderByDescending(entry => entry!["last_used_unix_ms"]!.GetValue<long>()))
        {
            var used = DateTimeOffset.FromUnixTimeMilliseconds(entry!["last_used_unix_ms"]!.GetValue<long>()).ToLocalTime();
            output.WriteLine($"  {Megabytes(Bytes(entry)),8}  last used {used.ToString("yyyy-MM-dd HH:mm", CultureInfo.InvariantCulture)}");
        }
        return 0;
    }

    private static int Clean(string directory, TextWriter output)
    {
        var report = RuntimeCache.Clear(directory);
        var remaining = report["entries"]!.AsArray();
        output.WriteLine($"Removed {Megabytes(Bytes(report["removed_bytes"]))} of compiled runtimes from {directory}.");
        if (remaining.Count == 0) return 0;
        output.WriteLine($"{Runtimes(remaining.Count)} ({Megabytes(remaining.Sum(Bytes))}) could not be removed; stop the sessions using them and run isoplane cache clean again:");
        foreach (var failure in report["failures"]!.AsArray())
            output.WriteLine("  " + failure!.GetValue<string>());
        return 1;
    }

    private static long Bytes(JsonNode? node) => node is JsonObject entry ? entry["bytes"]!.GetValue<long>() : node!.GetValue<long>();

    private static string Runtimes(int count) => count == 1 ? "1 compiled runtime" : $"{count} compiled runtimes";

    private static string Megabytes(long bytes) => (bytes / (1024.0 * 1024.0)).ToString("N0", CultureInfo.InvariantCulture) + " MB";
}
