using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.RegularExpressions;
using NuGet.Versioning;

/// <summary>
/// The release that added each documented item, kept in a JSON file beside the site. Pages are drafted
/// with a placeholder after each item's name. Resolving a page replaces the placeholder with a release
/// marker when the item arrived after the line's first release, with a <c>next</c> marker when no
/// release has stamped it yet, and with nothing otherwise, so that a line's original items carry no marker.
/// </summary>
internal sealed class Since
{
    private const char Delimiter = '\u0001';
    private static readonly Regex Placeholder = new($"{Delimiter}([=^])([^{Delimiter}]+){Delimiter}", RegexOptions.CultureInvariant);
    private static readonly JsonSerializerOptions Format = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase, WriteIndented = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private readonly Record record;

    private Since(Record record) => this.record = record;

    /// <summary>The placeholder for the item <paramref name="id"/>, written directly after its name.</summary>
    public static string Mark(string id) => $"{Delimiter}={id}{Delimiter}";

    /// <summary>
    /// The placeholder for a name that covers several items, such as a method with overloads: the
    /// marker of the earliest-added item whose id begins with <paramref name="prefix"/> and a hash.
    /// </summary>
    public static string MarkEarliest(string prefix) => $"{Delimiter}^{prefix}{Delimiter}";

    /// <summary>The ids of every item a set of drafted pages marks.</summary>
    public static HashSet<string> Items(IEnumerable<string> pages) => pages
        .SelectMany(page => Placeholder.Matches(page)).Where(match => match.Groups[1].Value == "=")
        .Select(match => match.Groups[2].Value).ToHashSet(StringComparer.Ordinal);

    public static Since Load(FileInfo file)
    {
        if (!file.Exists) return new Since(new Record(null, null, []));
        using var stream = file.OpenRead();
        var record = JsonSerializer.Deserialize<Record>(stream, Format)
            ?? throw new InvalidOperationException($"{file.FullName} holds no release record.");
        return new Since(record);
    }

    public void Save(FileInfo file)
    {
        var sorted = record with { Added = record.Added.OrderBy(item => item.Key, StringComparer.Ordinal).ToDictionary() };
        File.WriteAllText(file.FullName, JsonSerializer.Serialize(sorted, Format) + "\n");
    }

    /// <summary>
    /// Records <paramref name="version"/> as the release that added every item of <paramref name="items"/>
    /// the record does not list, and forgets items no page documents any more. A release on a new line
    /// becomes that line's first release.
    /// </summary>
    public void Stamp(string version, string line, IReadOnlySet<string> items)
    {
        var release = NuGetVersion.Parse(version);
        if (record.Line != line) (record.Line, record.First) = (line, version);
        else if (release < NuGetVersion.Parse(record.First!))
            throw new InvalidOperationException($"{version} precedes {record.First}, the first release of line {line}.");
        foreach (var item in items) record.Added.TryAdd(item, version);
        foreach (var item in record.Added.Keys.Except(items).ToArray()) record.Added.Remove(item);
    }

    public string Resolve(string page) => Placeholder.Replace(page, match =>
    {
        var version = match.Groups[1].Value == "=" ? record.Added.GetValueOrDefault(match.Groups[2].Value) : Earliest(match.Groups[2].Value + "#");
        if (version is null) return " :since[next]";
        return record.First is null || NuGetVersion.Parse(version) > NuGetVersion.Parse(record.First) ? $" :since[{version}]" : "";
    });

    private string? Earliest(string prefix) => record.Added
        .Where(item => item.Key.StartsWith(prefix, StringComparison.Ordinal))
        .Select(item => item.Value).MinBy(NuGetVersion.Parse);

    private sealed record Record(string? Line, string? First, Dictionary<string, string> Added)
    {
        [JsonPropertyOrder(0)] public string? Line { get; set; } = Line;
        [JsonPropertyOrder(1)] public string? First { get; set; } = First;
        [JsonPropertyOrder(2)] public Dictionary<string, string> Added { get; init; } = Added;
    }
}
