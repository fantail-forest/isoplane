using System.CommandLine;
using Isoplane.Cli;
using Isoplane.Native;

var telemetry = CommandTelemetry.Begin(args);
try
{
    return telemetry.Ended(await RunAsync());
}
catch (OperationCanceledException error) when (error.CancellationToken.IsCancellationRequested)
{
    return telemetry.Cancelled(130);
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
    var parsed = CliCommandLine.Create(Actions()).Parse(args.Length == 0 ? ["--help"] : args);
    if (parsed.Action is not System.CommandLine.Help.HelpAction && parsed.Errors.Count > 0)
        throw new ArgumentException(string.Join(Environment.NewLine, parsed.Errors.Select(error => error.Message)));
    // Each action handles Ctrl+C itself, and errors reach the handlers above.
    return await parsed.InvokeAsync(new InvocationConfiguration { ProcessTerminationTimeout = null, EnableDefaultExceptionHandler = false });
}

CliActions Actions() => new()
{
    Dev = _ => CommandCancellation.RunUntilStoppedAsync(token => Development.RunAsync(DevelopmentOptions.Parse(args[1..]), token), telemetry),
    DebugAdapter = _ => CommandCancellation.RunUntilStoppedAsync(token => Isoplane.Debugging.AdapterCommand.RunAsync(CliPaths.Home, token, new LocalDebugLauncher()), telemetry),
    Setup = parsed => CommandCancellation.RunOrRetryAsync(parsed.CommandResult.Command.Name, async token =>
    {
        using var client = new HttpClient(CertificateTrust.CreateHandler(allowAutoRedirect: true)) { Timeout = Timeout.InfiniteTimeSpan };
        client.DefaultRequestHeaders.UserAgent.ParseAdd($"isoplane/{ReleaseIdentity.OfRunningCli().Version}");
        return await Setup.RunAsync(SetupOptions.Parse(args), ReleaseIdentity.OfRunningCli(), CliPaths.Home, SetupMachine.Production(), client, token);
    }, Console.Error),
    UninstallSdk = parsed => CommandCancellation.RunOrRetryAsync(parsed.CommandResult.Command.Name,
        token => SdkRemoval.RunAsync(CliPaths.Home, dotnet => new DotnetRegistrations(dotnet, CliPaths.Home), token), Console.Error),
    CacheInfo = _ => Task.FromResult(CacheCommands.Info(CliPaths.RuntimeCache, Console.Out)),
    CacheClean = _ => Task.FromResult(CacheCommands.Clean(CliPaths.RuntimeCache, Console.Out)),
    PrepareRuntime = _ => Task.FromResult(RuntimePreparationCommand.Run(args, CliPaths.RuntimeCache, Console.Out)),
    Doctor = _ => CommandCancellation.RunUntilStoppedAsync(EnvironmentDoctor.RunAsync, telemetry),
    AgentInit = parsed => Task.FromResult(AgentCommands.Init(parsed)),
    AgentMcp = _ => CommandCancellation.RunUntilStoppedAsync(token => AgentServer.RunAsync(telemetry, token), telemetry),
    SkillsInstall = parsed => CommandCancellation.RunAsync(token =>
        SkillsCommands.InstallAsync(parsed, Console.Out, Console.IsInputRedirected ? null : SetupMachine.ConfirmOnTerminal, token)),
    Bundle = _ => CommandCancellation.RunAsync(async token =>
    {
        var options = BundleOptions.Parse(args[1..]);
        await ManagedBundle.CreateAsync(options, token);
        Console.WriteLine(options.Output);
        return 0;
    }),
    Daemon = parsed => CommandCancellation.RunUntilStoppedAsync(token => HostCommands.RunAsync(parsed, token), telemetry),
    Deploy = parsed => CommandCancellation.RunAsync(token => HostCommands.RunAsync(parsed, token)),
    TelemetryStatus = _ => Task.FromResult(TelemetryCommands.Status(TelemetryHost.OfRunningCli(), Console.Out)),
    TelemetryEnable = _ => Task.FromResult(TelemetryCommands.Enable(TelemetryHost.OfRunningCli(), Console.Out)),
    TelemetryDisable = _ => Task.FromResult(TelemetryCommands.Disable(TelemetryHost.OfRunningCli(), Console.Out)),
    Completions = parsed =>
    {
        Console.WriteLine(ShellCompletions.Script(Enum.Parse<CompletionShell>(parsed.GetValue<string>("shell")!, ignoreCase: true)));
        return Task.FromResult(0);
    },
    Complete = parsed =>
    {
        foreach (var candidate in ShellCompletions.Candidates(parsed.GetValue<string>("line")!)) Console.WriteLine(candidate);
        return Task.FromResult(0);
    },
};
