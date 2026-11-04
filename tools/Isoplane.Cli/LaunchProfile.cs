using System.Text.Json;

namespace Isoplane.Cli;

/// <summary>Limits a launch profile overrides for a development session.</summary>
internal sealed record LimitOverrides(ulong? MemoryBytes, Dictionary<string, ulong> ResourceLimits)
{
    internal static LimitOverrides None { get; } = new(null, []);
}

/// <summary>
/// The profile of <c>Properties/launchSettings.json</c> a development session
/// runs with: its HTTP port, the environment variables it supplies as
/// configuration, and Isoplane's own limit overrides.
/// </summary>
internal sealed record LaunchProfile(string Name, ushort? Port, IReadOnlyDictionary<string, string> Environment, LimitOverrides Limits)
{
    private static readonly string LimitsKey = LaunchProfileKeys.Limits.Name;

    /// <summary>The manifest's resource limit for each override a profile may name.</summary>
    private static readonly Dictionary<string, string> ResourceLimits = LaunchProfileKeys.LimitOverrides
        .Where(key => key.ResourceLimit is not null).ToDictionary(key => key.Name, key => key.ResourceLimit!, StringComparer.Ordinal);

    private static readonly JsonDocumentOptions Lenient = new() { CommentHandling = JsonCommentHandling.Skip, AllowTrailingCommas = true };

    /// <summary>
    /// Reads the selected profile, or the one <c>dotnet run</c> selected, or the
    /// first that launches the project. Returns null when the project has no
    /// launch settings or none launches the project.
    /// </summary>
    internal static LaunchProfile? Read(string directory, string? selected)
    {
        var path = Path.Combine(directory, "Properties", "launchSettings.json");
        if (!File.Exists(path))
            return selected is null ? null : throw new ArgumentException($"Launch profile '{selected}' was requested, but {path} does not exist.");
        selected ??= System.Environment.GetEnvironmentVariable("DOTNET_LAUNCH_PROFILE") is { Length: > 0 } inherited ? inherited : null;
        try
        {
            using var document = JsonDocument.Parse(File.ReadAllText(path), Lenient);
            if (!document.RootElement.TryGetProperty("profiles", out var profiles) || profiles.ValueKind != JsonValueKind.Object)
                return selected is null ? null : throw new ArgumentException($"{path} defines no launch profiles.");
            foreach (var profile in profiles.EnumerateObject())
            {
                if (selected is null ? LaunchesProject(profile.Value) : string.Equals(profile.Name, selected, StringComparison.OrdinalIgnoreCase))
                    return Parse(profile.Name, profile.Value, path);
            }
            return selected is null ? null : throw new ArgumentException($"{path} has no launch profile named '{selected}'.");
        }
        catch (JsonException error)
        {
            throw new ArgumentException($"Cannot read {path}: {error.Message}", error);
        }
    }

    private static bool LaunchesProject(JsonElement profile) =>
        profile.ValueKind == JsonValueKind.Object && profile.TryGetProperty(LaunchProfileKeys.Command.Name, out var command)
        && command.ValueKind == JsonValueKind.String && command.GetString() == "Project";

    private static LaunchProfile Parse(string name, JsonElement profile, string path)
    {
        if (!LaunchesProject(profile))
            throw new ArgumentException($"Launch profile '{name}' in {path} does not launch the project; its {LaunchProfileKeys.Command.Name} must be \"Project\".");
        var environment = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        if (profile.TryGetProperty(LaunchProfileKeys.Environment.Name, out var variables) && variables.ValueKind == JsonValueKind.Object)
            foreach (var variable in variables.EnumerateObject())
                environment[variable.Name] = variable.Value.ValueKind == JsonValueKind.String ? variable.Value.GetString()! : variable.Value.GetRawText();
        return new(name, HttpPort(name, profile, path), environment, Overrides(name, profile, path));
    }

    /// <summary>The first HTTP address's port. The session serves plain HTTP on loopback, so HTTPS addresses are passed over.</summary>
    private static ushort? HttpPort(string name, JsonElement profile, string path)
    {
        if (!profile.TryGetProperty(LaunchProfileKeys.Url.Name, out var urls) || urls.ValueKind != JsonValueKind.String) return null;
        foreach (var url in urls.GetString()!.Split(';', StringSplitOptions.TrimEntries | StringSplitOptions.RemoveEmptyEntries))
        {
            if (!Uri.TryCreate(url, UriKind.Absolute, out var address) || address.Scheme is not ("http" or "https"))
                throw new ArgumentException($"Launch profile '{name}' in {path} has an invalid {LaunchProfileKeys.Url.Name} '{url}'.");
            if (address.Scheme == "http") return checked((ushort)address.Port);
        }
        return null;
    }

    private static LimitOverrides Overrides(string name, JsonElement profile, string path)
    {
        if (!profile.TryGetProperty(LimitsKey, out var overrides)) return LimitOverrides.None;
        var supported = string.Join(", ", LaunchProfileKeys.LimitOverrides.Select(key => key.Name));
        if (overrides.ValueKind != JsonValueKind.Object)
            throw new ArgumentException($"{LimitsKey} in launch profile '{name}' in {path} must be an object with any of: {supported}.");
        ulong? memory = null;
        var limits = new Dictionary<string, ulong>(StringComparer.Ordinal);
        foreach (var limit in overrides.EnumerateObject())
        {
            if (limit.Value.ValueKind != JsonValueKind.Number || !limit.Value.TryGetUInt64(out var value) || value == 0)
                throw new ArgumentException($"{LimitsKey}.{limit.Name} in launch profile '{name}' in {path} must be a positive whole number.");
            if (limit.Name == LaunchProfileKeys.Memory.Name) memory = value;
            else if (ResourceLimits.TryGetValue(limit.Name, out var resource)) limits[resource] = value;
            else throw new ArgumentException($"{LimitsKey} in launch profile '{name}' in {path} does not support '{limit.Name}'; it supports: {supported}.");
        }
        return new(memory, limits);
    }
}
