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
    var parsed = CliCommandLine.Create().Parse(args.Length == 0 ? ["--help"] : args);
    if (parsed.Action is System.CommandLine.Help.HelpAction) return await parsed.InvokeAsync();
    if (parsed.Errors.Count > 0)
        throw new ArgumentException(string.Join(Environment.NewLine, parsed.Errors.Select(error => error.Message)));
    if (args[0] == "completions")
    {
        Console.WriteLine(ShellCompletions.Script(Enum.Parse<CompletionShell>(parsed.GetValue<string>("shell")!, ignoreCase: true)));
        return 0;
    }
    if (args[0] == "complete")
    {
        foreach (var candidate in ShellCompletions.Candidates(parsed.GetValue<string>("line")!)) Console.WriteLine(candidate);
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
