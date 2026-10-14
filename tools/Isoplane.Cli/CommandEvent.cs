using System.Globalization;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.RegularExpressions;

namespace Isoplane.Cli;

internal enum TelemetryCommand
{
    Dev, DebugAdapter, Setup, UninstallSdk, Doctor, DoctorModule, Cache, Bundle, Unknown
}

internal enum CommandMode { Run, Debug, Test }

internal enum CommandOutcome { Succeeded, Failed, Cancelled, Crashed }

internal enum CommandFailure { Usage, InvalidData, InvalidOperation, FileSystem, Process, Unexpected }

/// <summary>
/// One run of a command, as <c>tools/TelemetryProtocol/command-event.schema.json</c> defines it. Every
/// value is a number, a flag or a name from a fixed list, so nothing a user typed can be recorded.
/// </summary>
internal sealed record CommandEvent(
    Guid Id, string OccurredAt, Guid? InstallId, bool Ci, CodingAgent Agent, TelemetryCommand Command, CommandMode Mode,
    CommandOutcome Outcome, int? ExitCode, CommandFailure? Failure, long DurationMs, string CliVersion,
    string Platform, string OsArchitecture, string OsVersion, string? Distribution,
    string? DistributionVersion, string RuntimeVersion, string? Locale)
{
    internal const long LongestDurationMs = 2_592_000_000;

    internal static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web)
    {
        Converters = { new JsonStringEnumConverter(JsonNamingPolicy.KebabCaseLower, allowIntegerValues: false) }
    };

    public string Event => "command";

    internal static string Timestamp(DateTimeOffset moment) =>
        moment.UtcDateTime.ToString("yyyy-MM-dd'T'HH:mm:ss.fff'Z'", CultureInfo.InvariantCulture);

    internal string ToJson() => JsonSerializer.Serialize(this, Json);
}

internal static class CommandNames
{
    private static readonly Dictionary<string, TelemetryCommand> Named = Enum.GetValues<TelemetryCommand>()
        .Where(command => command is not (TelemetryCommand.DoctorModule or TelemetryCommand.Unknown))
        .ToDictionary(Name, StringComparer.Ordinal);

    internal static string Name<T>(T value) where T : struct, Enum =>
        JsonSerializer.Serialize(value, CommandEvent.Json).Trim('"');

    internal static bool AsksForHelp(string[] args) =>
        args.Length == 0 || args is ["--help"] or ["-h"] or [_, "--help" or "-h"];

    /// <summary>
    /// The name a run is recorded under, or null for a run that is not recorded: help, the version and the
    /// telemetry command itself. A first argument that names no command is <see cref="TelemetryCommand.Unknown"/>.
    /// </summary>
    internal static TelemetryCommand? Classify(string[] args)
    {
        if (AsksForHelp(args) || args is ["--version"] || args[0] == "telemetry") return null;
        if (args[0] == "doctor") return args.Length == 1 ? TelemetryCommand.Doctor : TelemetryCommand.DoctorModule;
        return Named.GetValueOrDefault(args[0], TelemetryCommand.Unknown);
    }

    internal static CommandMode Mode(string[] args, TelemetryCommand command, Func<string, string?> environment)
    {
        if (TelemetryVariables.IsOn(environment(TelemetryVariables.TestRun.Name))) return CommandMode.Test;
        return command is TelemetryCommand.DebugAdapter || (command is TelemetryCommand.Dev && args.Contains("--debug"))
            ? CommandMode.Debug : CommandMode.Run;
    }

    internal static CommandFailure Categorize(Exception error) => error switch
    {
        JsonException => CommandFailure.InvalidData,
        ArgumentException => CommandFailure.Usage,
        IOException or UnauthorizedAccessException => CommandFailure.FileSystem,
        System.ComponentModel.Win32Exception => CommandFailure.Process,
        InvalidOperationException => CommandFailure.InvalidOperation,
        _ => CommandFailure.Unexpected
    };
}

/// <summary>
/// The patterns the event schema gives its text fields. The service drops an event with one field that
/// does not fit, so a value the machine reports is left out when it does not.
/// </summary>
internal static class EventSchema
{
    private static readonly Dictionary<string, Regex> Patterns = Load();

    internal static string? Fitting(string property, string? value) =>
        value is not null && !value.Contains('\n') && Patterns[property].IsMatch(value) ? value : null;

    private static Dictionary<string, Regex> Load()
    {
        using var resource = typeof(EventSchema).Assembly.GetManifestResourceStream("command-event.schema.json")
            ?? throw new InvalidOperationException("The CLI package is missing its event schema; reinstall a complete package.");
        using var schema = JsonDocument.Parse(resource);
        return schema.RootElement.GetProperty("properties").EnumerateObject()
            .Where(property => property.Value.TryGetProperty("pattern", out _))
            .ToDictionary(property => property.Name, property => new Regex(property.Value.GetProperty("pattern").GetString()!, RegexOptions.CultureInvariant));
    }
}

/// <summary>A Linux distribution's identity, as its os-release file states it.</summary>
internal sealed record LinuxDistribution(string? Id, string? Version)
{
    // The os-release specification's order: the second is read only when the first is missing.
    private static readonly string[] Files = ["/etc/os-release", "/usr/lib/os-release"];

    internal static LinuxDistribution OfThisMachine() => OperatingSystem.IsLinux() ? Read(Files) : new(null, null);

    internal static LinuxDistribution Read(IEnumerable<string> files)
    {
        foreach (var file in files)
        {
            try { return Parse(File.ReadLines(file)); }
            catch (Exception error) when (error is FileNotFoundException or DirectoryNotFoundException) { }
            // A file that is there and cannot be read leaves the distribution unknown.
            catch (Exception error) when (error is IOException or UnauthorizedAccessException) { return new(null, null); }
        }
        return new(null, null);
    }

    internal static LinuxDistribution Parse(IEnumerable<string> lines)
    {
        var values = lines.Select(line => line.Split('=', 2)).Where(parts => parts.Length == 2)
            .GroupBy(parts => parts[0].Trim()).ToDictionary(group => group.Key, group => group.Last()[1].Trim().Trim('"', '\''));
        // The specification's default for a file that names no distribution.
        return new(EventSchema.Fitting("distribution", values.GetValueOrDefault("ID", "linux")),
            EventSchema.Fitting("distributionVersion", values.GetValueOrDefault("VERSION_ID")));
    }
}

/// <summary>What a command event says about the machine and the caller.</summary>
internal sealed record TelemetryContext(
    bool Ci, CodingAgent Agent, string CliVersion, string Platform, string OsArchitecture, string OsVersion,
    LinuxDistribution Distribution, string RuntimeVersion, string? Locale)
{
    internal static TelemetryContext Read(Func<string, string?> environment, string cliVersion) => new(
        TelemetryVariables.CiFlags.Any(name => TelemetryVariables.IsOn(environment(name)))
            || TelemetryVariables.CiMarkers.Any(name => environment(name) is { Length: > 0 }),
        TelemetryVariables.Agents.FirstOrDefault(agent => environment(agent.Variable) is { Length: > 0 }).Agent,
        cliVersion,
        CliPaths.PlatformIdentifier(),
        RuntimeInformation.OSArchitecture.ToString().ToLowerInvariant(),
        Environment.OSVersion.Version.ToString(),
        LinuxDistribution.OfThisMachine(),
        Environment.Version.ToString(),
        EventSchema.Fitting("locale", CultureInfo.CurrentUICulture.Name));
}
