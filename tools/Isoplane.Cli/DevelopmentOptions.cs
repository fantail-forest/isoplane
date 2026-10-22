using System.Net;

namespace Isoplane.Cli;

internal sealed record DevelopmentOptions(string Directory, ushort? Port, bool Watch, bool Json, bool ShutdownOnInputClose, bool Debug, string Configuration = "Debug", bool NoBuild = false, IPEndPoint? DebugLaunchEndpoint = null, string? LaunchProfile = null, bool UseLaunchProfile = true)
{
    internal static DevelopmentOptions Parse(string[] arguments)
    {
        var parsed = CliCommandLine.Parse(["dev", .. arguments]);
        var directory = Path.GetFullPath(parsed.GetValue<string>("--project") ?? Environment.CurrentDirectory);
        var configuration = parsed.GetValue<string>("--configuration") ?? "Debug";
        var noBuild = parsed.GetValue<bool>("--no-build");
        var watch = !parsed.GetValue<bool>("--no-watch");
        var json = parsed.GetValue<bool>("--json");
        var debug = parsed.GetValue<bool>("--debug");
        var shutdownOnInputClose = parsed.GetValue<bool>("--shutdown-on-stdin-close");
        var launchProfile = parsed.GetValue<string>("--launch-profile");
        var useLaunchProfile = !parsed.GetValue<bool>("--no-launch-profile");
        ushort? port = null;
        if (parsed.GetValue<string>("--port") is { } portText)
            port = ushort.TryParse(portText, out var parsedPort) ? parsedPort
                : throw new ArgumentException("--port must be between 0 and 65535.");
        IPEndPoint? debugLaunchEndpoint = null;
        if (parsed.GetValue<string>("--debug-launch-endpoint") is { } endpoint
            && (!IPEndPoint.TryParse(endpoint, out debugLaunchEndpoint)
                || !debugLaunchEndpoint.Address.Equals(IPAddress.Loopback) || debugLaunchEndpoint.Port == 0))
            throw new ArgumentException("--debug-launch-endpoint requires an IPv4 loopback endpoint with a nonzero port.");
        if (debugLaunchEndpoint is not null && !debug)
            throw new ArgumentException("--debug-launch-endpoint requires --debug.");
        if (noBuild && watch)
            throw new ArgumentException("--no-build requires --no-watch; compile changes explicitly before running again.");
        if (!System.IO.Directory.Exists(directory))
            throw new ArgumentException($"Project directory does not exist: {directory}");
        if (launchProfile is not null && !useLaunchProfile)
            throw new ArgumentException("Use either --launch-profile NAME or --no-launch-profile, not both.");
        if (System.IO.Directory.GetFiles(directory, "*.csproj").Length != 1)
            throw new ArgumentException($"Expected exactly one .NET project in {directory}; create one with dotnet new, or select its directory with --project.");
        return new(directory, port, watch, json, shutdownOnInputClose, debug, configuration, noBuild, debugLaunchEndpoint, launchProfile, useLaunchProfile);
    }
}
