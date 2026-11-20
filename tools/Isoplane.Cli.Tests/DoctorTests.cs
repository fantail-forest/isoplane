using System.Diagnostics;
using Isoplane.Cli;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class DoctorTests
{
    [Fact]
    public async Task Doctor_announces_checks_before_reporting_an_unusable_installation()
    {
        // Doctor reads the template packages registered with the user's .NET SDK. A .NET home of the
        // test's own has none, whatever is registered on the machine running the test.
        var home = Directory.CreateTempSubdirectory("isoplane-doctor-").FullName;
        var dotnetHome = Directory.CreateTempSubdirectory("isoplane-doctor-dotnet-").FullName;
        try
        {
            var start = new ProcessStartInfo("dotnet")
            {
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            start.ArgumentList.Add(typeof(CliPaths).Assembly.Location);
            start.ArgumentList.Add("doctor");
            start.Environment["ISOPLANE_HOME"] = home;
            start.Environment["ISOPLANE_TELEMETRY_OPTOUT"] = "1";
            start.Environment["DOTNET_CLI_HOME"] = dotnetHome;
            start.Environment["DOTNET_GENERATE_ASPNET_CERTIFICATE"] = "false";
            using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start doctor.");
            var output = process.StandardOutput.ReadToEndAsync();
            var errors = process.StandardError.ReadToEndAsync();
            using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
            try
            {
                await process.WaitForExitAsync(deadline.Token);
                var reported = await errors;
                Assert.True(process.ExitCode != 0, "Doctor must fail when the templates are not registered: " + reported);
                Assert.True(reported.StartsWith("Checking the .NET SDK and the native host library...", StringComparison.Ordinal),
                    "Doctor must announce its work before any check can fail: " + reported);
                int announced = reported.IndexOf("Checking project templates...", StringComparison.Ordinal);
                int failed = reported.IndexOf("Isoplane.Templates", StringComparison.Ordinal);
                Assert.True(announced >= 0 && failed > announced, "Doctor must name the check that fails before reporting its failure: " + reported);
                Assert.True(string.IsNullOrEmpty(await output), "A failed doctor must not report a ready SDK.");
                Assert.True(reported.Contains("isoplane setup", StringComparison.Ordinal), "An incomplete installation needs a repair command.");
                Assert.Empty(Directory.GetFileSystemEntries(home));
            }
            finally
            {
                if (!process.HasExited)
                {
                    process.Kill(entireProcessTree: true);
                    await process.WaitForExitAsync();
                }
            }
        }
        finally
        {
            Directory.Delete(home, recursive: true);
            Directory.Delete(dotnetHome, recursive: true);
        }
    }
}
