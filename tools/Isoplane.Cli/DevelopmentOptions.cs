namespace Wisp.Cli;

internal enum GuestRuntimeProfile { NativeAot, Mono }

internal sealed record DevelopmentOptions(string Directory, ushort? Port, bool Watch, bool Json, bool ShutdownOnInputClose, GuestRuntimeProfile? GuestRuntime, bool Debug, string Configuration = "Debug", bool NoBuild = false, string? Module = null)
{
    internal static DevelopmentOptions Parse(string[] arguments)
    {
        var configuration = "Debug";
        var noBuild = false;
        string? module = null;
        var directory = Environment.CurrentDirectory;
        ushort? port = null;
        var watch = true;
        var json = false;
        var debug = false;
        var shutdownOnInputClose = false;
        GuestRuntimeProfile? guestRuntime = null;
        for (var index = 0; index < arguments.Length; index++)
        {
            switch (arguments[index])
            {
                case "--configuration" when index + 1 < arguments.Length:
                case "-c" when index + 1 < arguments.Length:
                    configuration = arguments[++index];
                    break;
                case "--no-build":
                    noBuild = true;
                    break;
                case "--module" when index + 1 < arguments.Length:
                    module = Path.GetFullPath(arguments[++index]);
                    break;
                case "--project" when index + 1 < arguments.Length:
                    directory = Path.GetFullPath(arguments[++index]);
                    break;
                case "--port" when index + 1 < arguments.Length:
                    if (!ushort.TryParse(arguments[++index], out var parsedPort))
                        throw new ArgumentException("--port must be between 0 and 65535.");
                    port = parsedPort;
                    break;
                case "--guest-runtime" when index + 1 < arguments.Length:
                    guestRuntime = arguments[++index] switch
                    {
                        "mono" => GuestRuntimeProfile.Mono,
                        "nativeaot" => GuestRuntimeProfile.NativeAot,
                        _ => throw new ArgumentException("--guest-runtime must be mono or nativeaot.")
                    };
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
                case "--no-watch":
                    watch = false;
                    break;
                default:
                    throw new ArgumentException($"Invalid or incomplete option '{arguments[index]}'. Run isoplane --help.");
            }
        }
        if (noBuild && watch)
            throw new ArgumentException("--no-build requires --no-watch; use dotnet watch to own rebuilds.");
        if (module is not null && !noBuild)
            throw new ArgumentException("--module requires --no-build.");
        if (debug && noBuild)
            throw new ArgumentException("--debug cannot be combined with --no-build.");
        if (debug && watch)
            throw new ArgumentException("--debug requires --no-watch; each debug session handles one request.");
        if (debug && guestRuntime == GuestRuntimeProfile.NativeAot)
            throw new ArgumentException("--debug requires the Mono guest runtime.");
        if (!System.IO.Directory.Exists(directory))
            throw new ArgumentException($"Project directory does not exist: {directory}");
        if (!File.Exists(Path.Combine(directory, "wisp.toml")))
            throw new ArgumentException($"No wisp.toml in {directory}; create a project with dotnet new isoplane-web.");
        if (System.IO.Directory.GetFiles(directory, "*.csproj").Length != 1)
            throw new ArgumentException($"Expected exactly one .NET project in {directory}.");
        return new(directory, port, watch, json, shutdownOnInputClose, guestRuntime, debug, configuration, noBuild, module);
    }
}
