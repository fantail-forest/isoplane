using System.Globalization;
using Isoplane.Native;
using Isoplane.Native.Interop;

namespace Isoplane.Cli;

/// <summary><c>isoplane cache info</c> and <c>isoplane cache clean</c>, over the per-user cache of compiled runtimes.</summary>
internal static class CacheCommands
{
    /// <summary><c>isoplane cache info</c>: what <paramref name="directory"/> holds, without creating it.</summary>
    internal static int Info(string directory, TextWriter output)
    {
        var inventory = RuntimeCache.Inventory(directory);
        var entries = inventory.Entries;
        output.WriteLine($"Compiled runtime cache: {directory}");
        output.WriteLine(entries.Length == 0
            ? $"Empty. The first session compiles the runtime it needs; the cache keeps at most {Megabytes(inventory.BudgetBytes)}."
            : $"{Runtimes(entries.Length)}, {Megabytes(Bytes(entries))} of at most {Megabytes(inventory.BudgetBytes)}.");
        foreach (var entry in entries.OrderByDescending(entry => entry.last_used_unix_ms))
        {
            var used = DateTimeOffset.FromUnixTimeMilliseconds(checked((long)entry.last_used_unix_ms)).ToLocalTime();
            output.WriteLine($"  {Megabytes(entry.bytes),8}  last used {used.ToString("yyyy-MM-dd HH:mm", CultureInfo.InvariantCulture)}");
        }
        return 0;
    }

    /// <summary><c>isoplane cache clean</c>: removes the compiled runtimes in <paramref name="directory"/> that no session is using.</summary>
    internal static int Clean(string directory, TextWriter output)
    {
        var cleared = RuntimeCache.Clear(directory);
        var remaining = cleared.Remaining.Entries;
        output.WriteLine($"Removed {Megabytes(cleared.RemovedBytes)} of compiled runtimes from {directory}.");
        if (remaining.Length == 0) return 0;
        output.WriteLine($"{Runtimes(remaining.Length)} ({Megabytes(Bytes(remaining))}) could not be removed; stop the sessions using them and run isoplane cache clean again:");
        foreach (var failure in cleared.Failures)
            output.WriteLine("  " + failure);
        return 1;
    }

    private static string Runtimes(int count) => count == 1 ? "1 compiled runtime" : $"{count} compiled runtimes";

    private static ulong Bytes(CompiledRuntime[] entries) => entries.Aggregate(0UL, (total, entry) => checked(total + entry.bytes));

    private static string Megabytes(ulong bytes) => (bytes / (1024.0 * 1024.0)).ToString("N0", CultureInfo.InvariantCulture) + " MB";
}
