using Isoplane.Cli;
using Isoplane.Native;

var telemetry = CommandTelemetry.Begin(args);
try
{
    return telemetry.Ended(await RunAsync());
}
catch (OperationCanceledException error) when (error.CancellationToken.IsCancellationRequested)
{
    return telemetry.Cancelled(CommandCancellation.Report(args.Length > 0 ? args[0] : "", Console.Error));
}
catch (Exception error) when (error is System.Text.Json.JsonException or ArgumentException or InvalidOperationException or IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
{
    Console.Error.WriteLine($"isoplane: {error.Message}");
    return telemetry.Failed(error, 1);
}
catch (Exception) when (telemetry.Crashed())
{
    throw;
}

async Task<int> RunAsync()
{
    Console.OutputEncoding = new System.Text.UTF8Encoding(encoderShouldEmitUTF8Identifier: false);
    if (args is ["--version"])
    {
        var identity = ReleaseIdentity.OfRunningCli();
        Console.WriteLine($"isoplane {identity.Version}+{identity.SourceRevision}");
        return 0;
    }
    if (CommandNames.AsksForHelp(args))
    {
        Console.WriteLine("isoplane dev [--project DIRECTORY] [--configuration CONFIGURATION] [--port PORT] [--launch-profile NAME | --no-launch-profile] [--no-watch] [--no-build] [--debug] [--json] [--shutdown-on-stdin-close]\nThe port, environment variables and isoplaneLimitsOverride come from the selected profile of Properties/launchSettings.json; --port overrides its applicationUrl.\nEnvironment variables named ISOPLANE_APP_<Key> supply application settings, with __ for a colon; they override the profile and User Secrets.\n--debug preserves the debugger connection across requests and source rebuilds.\n--no-build requires --no-watch and uses existing compiled assemblies.\nisoplane debug-adapter\nServe the Debug Adapter Protocol on standard input/output.\nisoplane setup [--source RELEASE_DIRECTORY|URL] [--yes]\nCheck prerequisites and install matching project templates; --yes installs missing native prerequisites without prompting.\nisoplane uninstall-sdk\nRemove everything setup installed, keeping the CLI.\nisoplane cache info|clean\nShow or remove the compiled runtimes that sessions of every project share. They are kept in ISOPLANE_CACHE_DIR when it is set, else in your user cache directory.\nisoplane prepare-runtime --runtime DIRECTORY\nCompile the shared runtime in DIRECTORY, an SDK's tools/runtime folder, into that cache before the first session needs it.\nSet ISOPLANE_TARGET_DIR to keep each project's package and session files under that directory, outside the project.\nisoplane doctor\nCheck this machine: the .NET SDK, the native host library and the templates.\nisoplane doctor MODULE [--guest-memory-bytes BYTES] [--json]\nValidate a WebAssembly module against the platform's rules.\nisoplane --version");
        Console.WriteLine("isoplane bundle --output FILE [--project DIRECTORY] [--configuration CONFIGURATION]\nCompile managed deployment input as a managed application archive.");
        Console.WriteLine($"isoplane telemetry status|enable|disable\nShow or change whether anonymous usage data is collected. Setting {TelemetryVariables.OptOut.Name}=1 or {TelemetryVariables.DoNotTrack.Name}=1 also turns it off, and {TelemetryVariables.Debug.Name}=1 prints each event without recording or sending it.");
        return 0;
    }
    if (args[0] == "telemetry") return TelemetryCommands.Run(args, TelemetryHost.OfRunningCli(), Console.Out);
    if (args[0] == "cache") return CacheCommands.Run(args, CliPaths.RuntimeCache, Console.Out);
    if (args[0] == "prepare-runtime") return RuntimePreparationCommand.Run(args, CliPaths.RuntimeCache, Console.Out);
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
