using System.Globalization;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Isoplane.Cli;

internal enum TelemetryCommand
{
    Dev, DebugAdapter, Setup, UninstallSdk, Doctor, DoctorModule, Cache, Bundle, Unknown
}

internal enum CommandOutcome { Succeeded, Failed, Cancelled, Crashed }

internal enum CommandFailure { Usage, InvalidData, InvalidOperation, FileSystem, Process, Unexpected }

internal enum TelemetryPlatform
{
    [JsonStringEnumMemberName("linux-x64")] LinuxX64,
    [JsonStringEnumMemberName("linux-arm64")] LinuxArm64,
    [JsonStringEnumMemberName("linux-musl-x64")] LinuxMuslX64,
    [JsonStringEnumMemberName("linux-musl-arm64")] LinuxMuslArm64,
    [JsonStringEnumMemberName("osx-x64")] OsxX64,
    [JsonStringEnumMemberName("osx-arm64")] OsxArm64,
    [JsonStringEnumMemberName("win-x64")] WinX64,
    [JsonStringEnumMemberName("win-arm64")] WinArm64,
    Other
}

/// <summary>
/// One run of a command, as <c>tools/TelemetryProtocol/command-event.schema.json</c> defines it. Every
/// value is a number, a flag or a name from a fixed list, so nothing a user typed can be recorded.
/// </summary>
internal sealed record CommandEvent(
    Guid Id, string OccurredAt, Guid? InstallId, bool Ci, CodingAgent Agent, TelemetryCommand Command,
    CommandOutcome Outcome, int? ExitCode, CommandFailure? Failure, long DurationMs, string CliVersion,
    TelemetryPlatform Platform, string OsVersion, string RuntimeVersion, string? Locale)
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

/// <summary>What a command event says about the machine and the caller.</summary>
internal sealed record TelemetryContext(
    bool Ci, CodingAgent Agent, string CliVersion, TelemetryPlatform Platform, string OsVersion, string RuntimeVersion, string? Locale)
{
    private static readonly Dictionary<string, TelemetryPlatform> Platforms =
        Enum.GetValues<TelemetryPlatform>().ToDictionary(CommandNames.Name, StringComparer.Ordinal);

    internal static TelemetryContext Read(Func<string, string?> environment, string cliVersion) => new(
        TelemetryVariables.CiFlags.Any(name => TelemetryVariables.IsOn(environment(name)))
            || TelemetryVariables.CiMarkers.Any(name => environment(name) is { Length: > 0 }),
        TelemetryVariables.Agents.FirstOrDefault(agent => environment(agent.Variable) is { Length: > 0 }).Agent,
        cliVersion,
        Platforms.GetValueOrDefault(CliPaths.PlatformIdentifier(), TelemetryPlatform.Other),
        Environment.OSVersion.Version.ToString(),
        Environment.Version.ToString(),
        CultureInfo.CurrentUICulture.Name is { Length: > 0 } culture ? culture : null);
}
