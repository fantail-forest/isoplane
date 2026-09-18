#if ISOPLANE_COMPAT_PROBE
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.RegularExpressions;

namespace Isoplane.App;

/// <summary>
/// The BCL compatibility probe. Off by default so the shipped guest stays a
/// hello-world Handler and its size stays comparable to one. Turn it on with:
///
/// <code>guests/dotnet/build.sh -p:IsoplaneCompatProbe=true</code>
///
/// Everything referenced here is something the compatibility story says works.
/// Each line is a claim the module either compiles and executes or does not, and
/// <c>hostcheck</c> runs the result, so this file is the compatibility matrix
/// as a test rather than as an assertion in a document.
/// </summary>
public sealed record Quote(string Sku, decimal Price, DateTimeOffset At);

[JsonSourceGenerationOptions(WriteIndented = false)]
[JsonSerializable(typeof(Quote))]
internal sealed partial class QuoteContext : JsonSerializerContext { }

internal static partial class Compat
{
    // Regex: interpreted and source-generated only, never compiled to IL. The
    // source generator is the sanctioned path because it needs no
    // Reflection.Emit, which the sandbox does not allow.
    [GeneratedRegex(@"^/quote/(?<sku>[A-Z0-9-]+)$")]
    private static partial Regex SkuRoute();

    internal static string Exercise(string path)
    {
        // LINQ, in-memory.
        List<int> top = Enumerable.Range(1, 50)
            .Where(i => i % 7 == 0)
            .Select(i => i * 2)
            .OrderByDescending(i => i)
            .Take(3)
            .ToList();

        Match m = SkuRoute().Match(path);
        string sku = m.Success ? m.Groups["sku"].Value : "UNKNOWN";

        // System.Text.Json in source-gen mode, plus `decimal` and
        // DateTimeOffset formatting under InvariantGlobalization.
        var quote = new Quote(sku, 12.34m, DateTimeOffset.FromUnixTimeMilliseconds(1_724_000_000_000));
        string json = JsonSerializer.Serialize(quote, QuoteContext.Default.Quote);

        return json + "|" + string.Join(",", top);
    }
}
#endif
