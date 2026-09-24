using System.Globalization;
using System.Text.RegularExpressions;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Mono.Debugging.Client;

namespace Isoplane.Debugging;

internal readonly record struct BreakpointHitCondition(HitCountMode Mode, int Count)
{
    internal static BreakpointHitCondition Parse(string? expression)
    {
        if (string.IsNullOrWhiteSpace(expression)) return new(HitCountMode.GreaterThanOrEqualTo, 1);
        var match = Regex.Match(expression.Trim(), @"^(==|>=|<=|>|<|%|=)?\s*([1-9][0-9]*)$", RegexOptions.CultureInvariant);
        if (!match.Success || !int.TryParse(match.Groups[2].Value, NumberStyles.None, CultureInfo.InvariantCulture, out int count))
            throw new ProtocolException("Hit conditions require a positive count, optionally prefixed by ==, >=, <=, >, <, or %.");
        var mode = match.Groups[1].Value switch
        {
            "<" => HitCountMode.LessThan, "<=" => HitCountMode.LessThanOrEqualTo,
            ">" => HitCountMode.GreaterThan, ">=" => HitCountMode.GreaterThanOrEqualTo,
            "%" => HitCountMode.MultipleOf, _ => HitCountMode.EqualTo
        };
        return new(mode, count);
    }

    internal bool Matches(long hits) => Mode switch
    {
        HitCountMode.LessThan => hits < Count, HitCountMode.LessThanOrEqualTo => hits <= Count,
        HitCountMode.GreaterThan => hits > Count, HitCountMode.GreaterThanOrEqualTo => hits >= Count,
        HitCountMode.MultipleOf => hits % Count == 0, HitCountMode.EqualTo => hits == Count,
        _ => throw new InvalidOperationException("Unsupported breakpoint hit-count mode.")
    };
}
