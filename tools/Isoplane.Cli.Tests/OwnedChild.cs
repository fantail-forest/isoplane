using System.IO.Pipes;

namespace Isoplane.Cli.Tests;

internal static class OwnedChild
{
    internal static async Task<int> Main(string[] arguments)
    {
        if (arguments is ["--verify-sdk-registrations", var registrationScenario, var registrationDirectory, var templatePackage, var templateVersion])
        {
            await SdkRegistrationsTests.VerifyRegistrationsAsync(registrationScenario, registrationDirectory, templatePackage, templateVersion);
            return 0;
        }
        if (arguments is ["--verify-ca-bundle", var certificateScenario])
        {
            await CertificateTrustTests.VerifyBundleAsync(certificateScenario);
            return 0;
        }
        if (arguments is ["--debug-fixture", var state, var fixturePackage, var fixtureModule, var sourceProject, var artifacts])
            return await PreparedDebugSession.RunAsync(state, fixturePackage, fixtureModule,
                new Isoplane.Build.DebugProject(sourceProject, new() { ["Configuration"] = "Release", ["ArtifactsPath"] = artifacts }));
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
        if (arguments is ["--read-local-secrets", var secretsId])
        {
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(LocalSecrets.Read(secretsId)));
            return 0;
        }
        if (arguments is ["--export-machine-trust", var trustDirectory])
        {
            Console.WriteLine(System.Text.Json.JsonSerializer.Serialize(MachineTrust.Export(trustDirectory)));
            return 0;
        }
        if (arguments is ["--write-then-wait-for-input-to-close", .. var lines])
        {
            foreach (var line in lines) Console.WriteLine(line);
            await Console.In.ReadToEndAsync();
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
