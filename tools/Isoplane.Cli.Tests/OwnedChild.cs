using System.IO.Pipes;

namespace Isoplane.Cli.Tests;

internal static class OwnedChild
{
    internal static async Task<int> Main(string[] arguments)
    {
        if (arguments is ["--verify-ca-bundle", var certificateScenario])
        {
            await CertificateTrustTests.VerifyBundleAsync(certificateScenario);
            return 0;
        }
        if (arguments is ["--debug-fixture", var project, var package, var module])
            return await PreparedDebugSession.RunAsync(project, package, module);
        if (arguments is ["--run-process", .. var command])
        {
            using var cancellation = new CommandCancellation();
            try
            {
                return await Processes.RunAsync(
                    Environment.GetEnvironmentVariable("ISOPLANE_TEST_PROCESS_FIXTURE")
                        ?? throw new InvalidOperationException("Run task test:managed to prepare the process fixture."),
                    command, Environment.CurrentDirectory,
                    cancellation.Token);
            }
            catch (OperationCanceledException) when (cancellation.Token.IsCancellationRequested) { return 0; }
        }
        if (arguments is ["--successful-doctor-probe"])
        {
            await Processes.CheckAsync("dotnet", [typeof(OwnedChild).Assembly.Location, "--doctor-tool", "0"],
                AppContext.BaseDirectory, CancellationToken.None);
            return 0;
        }
        if (arguments is ["--selected-sdk"])
        {
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(20));
            try { Console.WriteLine((await Readiness.SelectedSdkAsync(deadline.Token)).Version); return 0; }
            catch (InvalidOperationException error) { Console.Error.WriteLine(error.Message); return 1; }
        }
        if (arguments is ["--doctor-tool", var toolExit])
        {
            Console.WriteLine("version-detail");
            Console.Error.WriteLine("missing-library");
            return int.Parse(toolExit);
        }
        if (arguments is ["--read-local-secrets", var secretsId])
        {
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(LocalSecrets.Read(secretsId)));
            return 0;
        }
        if (arguments is not ["--owned-child", var pipeName])
            return 1;
        using var pipe = new NamedPipeClientStream(".", pipeName, PipeDirection.Out, PipeOptions.Asynchronous);
        await pipe.ConnectAsync();
        using var writer = new StreamWriter(pipe) { AutoFlush = true };
        await writer.WriteLineAsync(Environment.ProcessId.ToString());
        await Task.Delay(Timeout.Infinite);
        return 0;
    }
}
