using System.Net;

namespace Isoplane.Cli;

internal sealed record DevelopmentOptions(string Directory, ushort? Port, bool Watch, bool Json, bool ShutdownOnInputClose, bool Debug, string Configuration = "Debug", bool NoBuild = false, IPEndPoint? DebugLaunchEndpoint = null, string? LaunchProfile = null, bool UseLaunchProfile = true)
{
    internal static DevelopmentOptions Parse(string[] arguments)
    {
        IPEndPoint? debugLaunchEndpoint = null;
        var configuration = "Debug";
        var noBuild = false;
        var directory = Environment.CurrentDirectory;
        ushort? port = null;
        var watch = true;
        var json = false;
        var debug = false;
        var shutdownOnInputClose = false;
        string? launchProfile = null;
        var useLaunchProfile = true;
        for (var index = 0; index < arguments.Length; index++)
        {
            switch (arguments[index])
            {
                case "--configuration" when index + 1 < arguments.Length:
                case "-c" when index + 1 < arguments.Length:
                    configuration = arguments[++index];
                    break;
                case "--project" when index + 1 < arguments.Length:
                    directory = Path.GetFullPath(arguments[++index]);
                    break;
                case "--port" when index + 1 < arguments.Length:
                    if (!ushort.TryParse(arguments[++index], out var parsedPort))
                        throw new ArgumentException("--port must be between 0 and 65535.");
                    port = parsedPort;
                    break;
                case "--debug-launch-endpoint" when index + 1 < arguments.Length:
                    if (!IPEndPoint.TryParse(arguments[++index], out debugLaunchEndpoint)
                        || !debugLaunchEndpoint.Address.Equals(IPAddress.Loopback) || debugLaunchEndpoint.Port == 0)
                        throw new ArgumentException("--debug-launch-endpoint requires an IPv4 loopback endpoint with a nonzero port.");
                    break;
                case "--launch-profile" when index + 1 < arguments.Length:
                    launchProfile = arguments[++index];
                    break;
                case "--no-launch-profile":
                    useLaunchProfile = false;
                    break;
                case "--debug":
                    debug = true;
                    break;
                case "--json":
                    json = true;
                    break;
                case "--shutdown-on-stdin-close":
                    shutdownOnInputClose = true;
                    break;
                case "--no-build":
                    noBuild = true;
                    break;
                case "--no-watch":
                    watch = false;
                    break;
                default:
                    throw new ArgumentException($"Invalid or incomplete option '{arguments[index]}'. Run isoplane --help.");
            }
        }
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
