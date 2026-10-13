using Isoplane.Cli;
using Isoplane.Native;

try
{
    Console.OutputEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false);
    if (args is ["--version"])
    {
        var identity = ReleaseIdentity.OfRunningCli();
        Console.WriteLine($"isoplane {identity.Version}+{identity.SourceRevision}");
        return 0;
    }
    if (args.Length == 0 || args is ["--help"] or ["-h"] or [_, "--help" or "-h"])
    {
        Console.WriteLine("isoplane dev [--project DIRECTORY] [--configuration CONFIGURATION] [--port PORT] [--launch-profile NAME | --no-launch-profile] [--no-watch] [--no-build] [--debug] [--json] [--shutdown-on-stdin-close]\nThe port, environment variables and isoplaneLimitsOverride come from the selected profile of Properties/launchSettings.json; --port overrides its applicationUrl.\nEnvironment variables named ISOPLANE_APP_<Key> supply application settings, with __ for a colon; they override the profile and User Secrets.\n--debug preserves the debugger connection across requests and source rebuilds.\n--no-build requires --no-watch and uses existing compiled assemblies.\nisoplane debug-adapter\nServe the Debug Adapter Protocol on standard input/output.\nisoplane setup [--source RELEASE_DIRECTORY|URL] [--yes]\nCheck prerequisites and install matching project templates; --yes installs missing native prerequisites without prompting.\nisoplane uninstall-sdk\nRemove everything setup installed, keeping the CLI.\nisoplane cache info|clean\nShow or remove the compiled runtimes that sessions of every project share. They are kept in ISOPLANE_CACHE_DIR when it is set, else in your user cache directory.\nSet ISOPLANE_TARGET_DIR to keep each project's package and session files under that directory, outside the project.\nisoplane doctor\nCheck this machine: the .NET SDK, the native host library and the templates.\nisoplane doctor MODULE [--guest-memory-bytes BYTES] [--json]\nValidate a WebAssembly module against the platform's rules.\nisoplane --version");
        Console.WriteLine("isoplane bundle --output FILE [--project DIRECTORY] [--configuration CONFIGURATION]\nCompile managed deployment input as a managed application archive.");
        return 0;
    }
    if (args[0] == "cache") return CacheCommands.Run(args, CliPaths.RuntimeCache, Console.Out);
    using var cancellation = new CommandCancellation();
    if (args is ["debug-adapter"]) return await Isoplane.Debugging.AdapterCommand.RunAsync(cancellation.Token, new LocalDebugLauncher());
    if (args[0] == "setup")
    {
        using var client = new HttpClient(CertificateTrust.CreateHandler(allowAutoRedirect: true)) { Timeout = Timeout.InfiniteTimeSpan };
        client.DefaultRequestHeaders.UserAgent.ParseAdd($"isoplane/{ReleaseIdentity.OfRunningCli().Version}");
        return await Setup.RunAsync(SetupOptions.Parse(args), ReleaseIdentity.OfRunningCli(), CliPaths.Home, SetupMachine.Production(), client, cancellation.Token);
    }
    if (args is ["uninstall-sdk"])
        return await SdkRemoval.RunAsync(CliPaths.Home, dotnet => new DotnetRegistrations(dotnet, CliPaths.Home), cancellation.Token);
    if (args is ["doctor"]) return await EnvironmentDoctor.RunAsync(cancellation.Token);
    if (args[0] == "doctor") return ModuleDoctor.Run(args);
    if (args[0] == "bundle")
    {
        var options = BundleOptions.Parse(args[1..]);
        await ManagedBundle.CreateAsync(options, cancellation.Token);
        Console.WriteLine(options.Output);
        return 0;
    }
    return args[0] switch
    {
        "dev" => await Development.RunAsync(DevelopmentOptions.Parse(args[1..]), cancellation.Token),
        _ => throw new ArgumentException($"Unknown command '{args[0]}'. Run isoplane --help.")
    };
}
catch (OperationCanceledException error) when (error.CancellationToken.IsCancellationRequested)
{
    return CommandCancellation.Report(args.Length > 0 ? args[0] : "", Console.Error);
}
catch (Exception error) when (error is System.Text.Json.JsonException or ArgumentException or InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
{
    Console.Error.WriteLine($"isoplane: {error.Message}");
    return 1;
}
