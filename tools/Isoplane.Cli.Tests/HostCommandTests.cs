using System.Diagnostics;
using System.Text.Json;
using Isoplane.TestSupport;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class HostCommandTests
{
    [Fact]
    public async Task local_deployment_passes_literal_arguments_to_the_installed_host_and_preserves_failure()
    {
        using var fixture = new HostFixture();
        using var cli = fixture.Start("deploy", "--local", "--config", "host path.toml", "--app", "api", "--bundle", "bundle; literal.tar");
        try
        {
            var output = cli.StandardOutput.ReadToEndAsync(fixture.Cancellation);
            var error = cli.StandardError.ReadToEndAsync(fixture.Cancellation);
            await cli.WaitForExitAsync(fixture.Cancellation);
            Assert.Equal("", await error);
            Assert.Equal(23, cli.ExitCode);
            var arguments = JsonSerializer.Deserialize<string[]>(await output);
            Assert.NotNull(arguments);
            Assert.Equal(["deploy", "--config", "host path.toml", "--app", "api", "--bundle", "bundle; literal.tar"], arguments);
        }
        finally { await fixture.StopAsync(cli); }
    }

    [PosixFact]
    public async Task cancelling_the_daemon_command_waits_for_the_host_to_shut_down()
    {
        using var fixture = new HostFixture();
        using var cli = fixture.Start("daemon", "--config", "host path.toml");
        try
        {
            var firstLine = await cli.StandardOutput.ReadLineAsync(fixture.Cancellation);
            Assert.NotNull(firstLine);
            var arguments = JsonSerializer.Deserialize<string[]>(firstLine);
            Assert.NotNull(arguments);
            Assert.Equal(["serve", "--config", "host path.toml", "--shutdown-on-stdin-close"], arguments);
            var output = cli.StandardOutput.ReadToEndAsync(fixture.Cancellation);
            var error = cli.StandardError.ReadToEndAsync(fixture.Cancellation);
            using var signal = Process.Start("kill", ["-TERM", cli.Id.ToString()])
                ?? throw new InvalidOperationException("Could not signal the daemon command.");
            await signal.WaitForExitAsync(fixture.Cancellation);
            Assert.Equal(0, signal.ExitCode);
            await cli.WaitForExitAsync(fixture.Cancellation);
            Assert.Equal("", await error);
            Assert.Equal(0, cli.ExitCode);
            Assert.Equal("host stopped", (await output).Trim());
        }
        finally { await fixture.StopAsync(cli); }
    }

    private sealed class HostFixture : IDisposable
    {
        private readonly FileSystemTestDirectory directory = new("isoplane-host-command-");
        private readonly CancellationTokenSource deadline = new(TimeSpan.FromSeconds(30));
        internal CancellationToken Cancellation => deadline.Token;

        internal HostFixture()
        {
            var executable = Environment.GetEnvironmentVariable("ISOPLANE_TEST_PROCESS_FIXTURE")
                ?? throw new InvalidOperationException("Run task test:managed to prepare the process fixture.");
            foreach (var file in Directory.EnumerateFiles(Path.GetDirectoryName(executable)!))
                File.Copy(file, Path.Combine(directory.Path, Path.GetFileName(file)));
            File.Move(Path.Combine(directory.Path, Path.GetFileName(executable)),
                Path.Combine(directory.Path, OperatingSystem.IsWindows() ? "isoplaned.exe" : "isoplaned"));
        }

        internal Process Start(params string[] arguments)
        {
            var start = new ProcessStartInfo("dotnet")
            {
                UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true,
                WorkingDirectory = directory.Path
            };
            start.Environment["PATH"] = directory.Path + Path.PathSeparator + start.Environment["PATH"];
            start.Environment["ISOPLANE_HOME"] = Path.Combine(directory.Path, "home");
            start.Environment["ISOPLANE_TELEMETRY_OPTOUT"] = "1";
            start.ArgumentList.Add(typeof(CliPaths).Assembly.Location);
            foreach (var argument in arguments) start.ArgumentList.Add(argument);
            return Process.Start(start) ?? throw new InvalidOperationException("Could not start the CLI.");
        }

        internal async Task StopAsync(Process process)
        {
            if (process.HasExited) return;
            process.Kill(entireProcessTree: true);
            await process.WaitForExitAsync(Cancellation);
        }

        public void Dispose()
        {
            deadline.Dispose();
            directory.Dispose();
        }
    }
}
