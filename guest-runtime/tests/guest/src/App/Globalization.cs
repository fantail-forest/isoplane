using System;
using System.Collections.Generic;
using System.Globalization;
using System.Text;
using System.Threading.Tasks;

namespace Acceptance;

/// <summary>
/// Globalization under the SDK's invariant mode (no ICU, no culture data)
/// and time zones on a disk that carries no tz database: what the culture
/// APIs answer, how strings compare, and which time-zone operations work
/// without data.
/// </summary>
internal sealed class Globalization
{
    public Task<string> RunAsync(string[] path) => Task.FromResult(Run(path));

    private static string Run(string[] path)
    {
        switch (path[0])
        {
            case "culture":
                return Culture();
            case "format":
                return Format();
            case "parse":
                return Parse();
            case "compare":
                return Compare();
            case "casing":
                return Casing();
            case "normalize":
                return Normalize();
            case "encodings":
                return Encodings();
            case "timezone":
                return TimeZones();
            case "timezone-custom":
                return CustomTimeZone();
            case "clock":
                return Clock();
            default:
                throw new ArgumentException("unknown globalization case " + path[0]);
        }
    }

    private static string Culture()
    {
        var parts = new List<string>
        {
            "current=" + Quote(CultureInfo.CurrentCulture.Name),
            "ui=" + Quote(CultureInfo.CurrentUICulture.Name),
            "invariant=" + CultureInfo.CurrentCulture.Equals(CultureInfo.InvariantCulture),
            "known=" + CultureInfo.GetCultures(CultureTypes.AllCultures).Length.ToString(CultureInfo.InvariantCulture),
        };
        parts.Add("de-DE=" + Attempt(() => Quote(CultureInfo.GetCultureInfo("de-DE").Name)));
        parts.Add("en-US=" + Attempt(() => Quote(new CultureInfo("en-US").Name)));
        parts.Add("region=" + Attempt(() => RegionInfo.CurrentRegion.TwoLetterISORegionName));
        return string.Join(" ", parts);
    }

    private static string Format()
    {
        var when = new DateTime(2026, 9, 11, 13, 5, 7, DateTimeKind.Utc);
        return string.Join("|",
            1234567.891.ToString("N2", CultureInfo.CurrentCulture),
            (-0.5).ToString("P1", CultureInfo.CurrentCulture),
            1234.567m.ToString("C", CultureInfo.CurrentCulture),
            when.ToString("o", CultureInfo.CurrentCulture),
            when.ToString("D", CultureInfo.CurrentCulture),
            when.ToString("g", CultureInfo.CurrentCulture),
            when.ToString("MMMM ddd", CultureInfo.CurrentCulture),
            TimeSpan.FromMinutes(90.5).ToString("c", CultureInfo.CurrentCulture),
            string.Format(CultureInfo.CurrentCulture, "{0:x8} {1,6:F3}", 48879, Math.PI));
    }

    private static string Parse()
    {
        var parts = new List<string>
        {
            decimal.Parse("1,234.56", NumberStyles.Number, CultureInfo.CurrentCulture).ToString(CultureInfo.InvariantCulture),
            double.TryParse("1.234,56", NumberStyles.Float | NumberStyles.AllowThousands, CultureInfo.CurrentCulture, out double german)
                ? german.ToString(CultureInfo.InvariantCulture)
                : "unparsed",
            DateTime.ParseExact("11/09/2026 13:05", "dd/MM/yyyy HH:mm", CultureInfo.CurrentCulture).ToString("s", CultureInfo.InvariantCulture),
            DateTime.Parse("September 11, 2026", CultureInfo.CurrentCulture).ToString("s", CultureInfo.InvariantCulture),
            DateTimeOffset.Parse("2026-09-11T13:05:07+02:00", CultureInfo.CurrentCulture).ToUniversalTime().ToString("s", CultureInfo.InvariantCulture),
            int.Parse("  42 ", NumberStyles.Integer, CultureInfo.CurrentCulture).ToString(CultureInfo.InvariantCulture),
        };
        return string.Join("|", parts);
    }

    private static string Compare()
    {
        CompareInfo invariant = CultureInfo.InvariantCulture.CompareInfo;
        return string.Join("|",
            "a-vs-B=" + Math.Sign(string.Compare("a", "B", StringComparison.InvariantCulture)),
            "a-vs-B-ignore-case=" + Math.Sign(string.Compare("a", "B", StringComparison.InvariantCultureIgnoreCase)),
            "sharp-s=" + Math.Sign(invariant.Compare("Straße", "Strasse", CompareOptions.None)),
            "ae-ligature=" + "encyclopædia".IndexOf("ae", StringComparison.InvariantCulture),
            "composed=" + string.Equals("\u00C5", "A\u030A", StringComparison.InvariantCulture),
            "ignore-symbols=" + Attempt(() => Math.Sign(invariant.Compare("co-op", "coop", CompareOptions.IgnoreSymbols)).ToString(CultureInfo.InvariantCulture)),
            "sorted=" + string.Join(",", Sorted("banana", "Apple", "cherry", "apple")),
            "prefix=" + invariant.IsPrefix("Wisp", "wi", CompareOptions.IgnoreCase),
            "sort-key=" + Attempt(() => Convert.ToHexString(invariant.GetSortKey("ab").KeyData).ToLowerInvariant()));
    }

    private static IEnumerable<string> Sorted(params string[] words)
    {
        var list = new List<string>(words);
        list.Sort(StringComparer.InvariantCulture);
        return list;
    }

    private static string Casing()
    {
        return string.Join("|",
            "straße".ToUpper(CultureInfo.CurrentCulture),
            "İstanbul".ToLower(CultureInfo.CurrentCulture).Length.ToString(CultureInfo.InvariantCulture),
            "ǆ".ToUpperInvariant(),
            "σς".ToUpperInvariant(),
            CultureInfo.CurrentCulture.TextInfo.ToTitleCase("hello wisp world"),
            char.ToUpperInvariant('ñ').ToString());
    }

    private static string Normalize()
    {
        return "composed=" + Attempt(() => ("A\u030A".Normalize(NormalizationForm.FormC) == "\u00C5").ToString())
            + " decomposed=" + Attempt(() => "\u00C5".Normalize(NormalizationForm.FormD).Length.ToString(CultureInfo.InvariantCulture))
            + " is-normalized=" + Attempt(() => "abc".IsNormalized().ToString())
            + " idn=" + Attempt(() => new IdnMapping().GetAscii("bücher.example"));
    }

    private static string Encodings()
    {
        return string.Join("|",
            "registered=" + Encoding.GetEncodings().Length.ToString(CultureInfo.InvariantCulture),
            "latin1=" + Attempt(() => Encoding.GetEncoding("iso-8859-1").CodePage.ToString(CultureInfo.InvariantCulture)),
            "cp1252=" + Attempt(() => Encoding.GetEncoding(1252).EncodingName),
            "utf8=" + Encoding.UTF8.GetByteCount("héllo wörld").ToString(CultureInfo.InvariantCulture),
            "utf16=" + Encoding.Unicode.GetByteCount("héllo").ToString(CultureInfo.InvariantCulture),
            "ascii-lossy=" + Encoding.ASCII.GetString(Encoding.ASCII.GetBytes("héllo")));
    }

    private static string TimeZones()
    {
        var parts = new List<string>
        {
            "utc=" + TimeZoneInfo.Utc.Id,
            "local=" + TimeZoneInfo.Local.Id + "/" + TimeZoneInfo.Local.BaseUtcOffset.ToString("c", CultureInfo.InvariantCulture),
            "local-is-utc=" + TimeZoneInfo.Local.Equals(TimeZoneInfo.Utc),
            "system-zones=" + TimeZoneInfo.GetSystemTimeZones().Count.ToString(CultureInfo.InvariantCulture),
            "london=" + Attempt(() => TimeZoneInfo.FindSystemTimeZoneById("Europe/London").Id),
            "london-offset=" + Attempt(() => TimeZoneInfo.FindSystemTimeZoneById("Europe/London")
                .GetUtcOffset(new DateTime(2026, 9, 11, 12, 0, 0, DateTimeKind.Utc)).ToString("c", CultureInfo.InvariantCulture)),
            "utc-by-id=" + Attempt(() => TimeZoneInfo.FindSystemTimeZoneById("UTC").Id),
            "try-london=" + TimeZoneInfo.TryFindSystemTimeZoneById("Europe/London", out _),
            "iana-to-windows=" + TimeZoneInfo.TryConvertIanaIdToWindowsId("Europe/London", out string? windowsId) + "/" + Quote(windowsId ?? ""),
            "windows-to-iana=" + TimeZoneInfo.TryConvertWindowsIdToIanaId("GMT Standard Time", out string? ianaId) + "/" + Quote(ianaId ?? ""),
        };
        return string.Join(" ", parts);
    }

    /// <summary>
    /// A zone built by the application, with a daylight rule, needs no tz
    /// database; conversions through it and through the built-in UTC zone.
    /// </summary>
    private static string CustomTimeZone()
    {
        var transitionIn = TimeZoneInfo.TransitionTime.CreateFloatingDateRule(new DateTime(1, 1, 1, 2, 0, 0), 3, 5, DayOfWeek.Sunday);
        var transitionOut = TimeZoneInfo.TransitionTime.CreateFloatingDateRule(new DateTime(1, 1, 1, 3, 0, 0), 10, 5, DayOfWeek.Sunday);
        var rule = TimeZoneInfo.AdjustmentRule.CreateAdjustmentRule(
            DateTime.MinValue.Date, DateTime.MaxValue.Date, TimeSpan.FromHours(1), transitionIn, transitionOut);
        TimeZoneInfo zone = TimeZoneInfo.CreateCustomTimeZone(
            "Wisp Central", TimeSpan.FromHours(1), "Wisp Central", "Wisp Standard", "Wisp Summer", new[] { rule });
        var summerUtc = new DateTime(2026, 9, 11, 12, 0, 0, DateTimeKind.Utc);
        var winterUtc = new DateTime(2026, 12, 11, 12, 0, 0, DateTimeKind.Utc);
        DateTime summerLocal = TimeZoneInfo.ConvertTimeFromUtc(summerUtc, zone);
        DateTime winterLocal = TimeZoneInfo.ConvertTimeFromUtc(winterUtc, zone);
        DateTime back = TimeZoneInfo.ConvertTimeToUtc(summerLocal, zone);
        var offset = new DateTimeOffset(2026, 9, 11, 14, 0, 0, TimeSpan.FromHours(2));
        return string.Join("|",
            summerLocal.ToString("s", CultureInfo.InvariantCulture) + "/" + zone.IsDaylightSavingTime(summerLocal),
            winterLocal.ToString("s", CultureInfo.InvariantCulture) + "/" + zone.IsDaylightSavingTime(winterLocal),
            back.ToString("s", CultureInfo.InvariantCulture) + "/" + back.Kind,
            TimeZoneInfo.ConvertTimeBySystemTimeZoneId(summerUtc, "UTC").ToString("s", CultureInfo.InvariantCulture),
            offset.UtcDateTime.ToString("s", CultureInfo.InvariantCulture),
            TimeZoneInfo.ConvertTime(offset, zone).ToString("yyyy-MM-ddTHH:mm:ssK", CultureInfo.InvariantCulture),
            zone.GetUtcOffset(summerUtc).ToString("c", CultureInfo.InvariantCulture));
    }

    /// <summary>The guest's wall clock is the host's, and local time is UTC.</summary>
    private static string Clock()
    {
        DateTime utc = DateTime.UtcNow;
        DateTime local = DateTime.Now;
        return string.Join("|",
            utc.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture),
            utc.Kind.ToString(),
            local.Kind.ToString(),
            (local - utc).ToString("c", CultureInfo.InvariantCulture),
            DateTimeOffset.Now.Offset.ToString("c", CultureInfo.InvariantCulture),
            TimeZoneInfo.Local.GetUtcOffset(utc).ToString("c", CultureInfo.InvariantCulture));
    }

    private static string Attempt(Func<string> operation)
    {
        try
        {
            return operation();
        }
        catch (Exception error)
        {
            return error.GetType().Name;
        }
    }

    private static string Quote(string text) => "'" + text + "'";
}
