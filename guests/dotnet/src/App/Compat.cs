#if WISP_COMPAT_PROBE
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.RegularExpressions;

namespace Wisp.App;

/// <summary>
/// The §12.1 compatibility probe. Off by default so the shipped guest stays a
/// hello-world Handler and its size can be compared honestly against §4.2's
/// 2.5 MB target. Turn it on with:
///
/// <code>guests/dotnet/build.sh -p:WispCompatProbe=true</code>
///
/// Everything referenced here is something §12.1 claims works. Each line is a
/// claim the module either compiles and executes or does not, and
/// <c>hostcheck</c> runs the result, so this file is the compatibility matrix
/// as a test rather than as an assertion in a document.
/// </summary>
public sealed record Quote(string Sku, decimal Price, DateTimeOffset At);

[JsonSourceGenerationOptions(WriteIndented = false)]
[JsonSerializable(typeof(Quote))]
internal sealed partial class QuoteContext : JsonSerializerContext { }

internal static partial class Compat
{
    // §12.1: Regex "interpreted + source-gen; no compiled-to-IL". The
    // source generator is the sanctioned path because it needs no
    // Reflection.Emit, which §2.5.3 forbids.
    [GeneratedRegex(@"^/quote/(?<sku>[A-Z0-9-]+)$")]
    private static partial Regex SkuRoute();

    internal static string Exercise(string path)
    {
        // LINQ, in-memory (§12.1).
        List<int> top = Enumerable.Range(1, 50)
            .Where(i => i % 7 == 0)
            .Select(i => i * 2)
            .OrderByDescending(i => i)
            .Take(3)
            .ToList();

        Match m = SkuRoute().Match(path);
        string sku = m.Success ? m.Groups["sku"].Value : "UNKNOWN";

        // System.Text.Json in source-gen mode (§12.1), plus `decimal` and
        // DateTimeOffset formatting under InvariantGlobalization.
        var quote = new Quote(sku, 12.34m, DateTimeOffset.FromUnixTimeMilliseconds(1_724_000_000_000));
        string json = JsonSerializer.Serialize(quote, QuoteContext.Default.Quote);

        return json + "|" + string.Join(",", top);
    }
}
#endif
