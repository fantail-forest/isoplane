using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class WorkloadInstallationTests
{
    [Fact]
    public async Task Sdk_discovery_honors_the_projects_global_json()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-sdk-selection-");
        try
        {
            await File.WriteAllTextAsync(Path.Combine(directory.FullName, "global.json"),
                "{\"sdk\":{\"version\":\"999.0.100\",\"rollForward\":\"disable\"}}");
            var start = CredentialTestChild.StartInfo("dotnet", typeof(OwnedChild).Assembly.Location, "--selected-sdk");
            start.WorkingDirectory = directory.FullName;
            start.Environment["ISOPLANE_HOME"] = Path.Combine(directory.FullName, "home");
            using var process = System.Diagnostics.Process.Start(start) ?? throw new InvalidOperationException("Cannot start SDK discovery.");
            var output = process.StandardOutput.ReadToEndAsync();
            var errors = process.StandardError.ReadToEndAsync();
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
            try
            {
                await process.WaitForExitAsync(deadline.Token);
                Assert.True(process.ExitCode != 0 && (await errors).Contains("999.0.100", StringComparison.Ordinal),
                    "Doctor and setup must report the project's unavailable SDK rather than silently selecting a different SDK from a temporary directory.");
                Assert.True(string.IsNullOrEmpty(await output), "An unavailable project SDK must not be reported as selected.");
            }
            finally
            {
                if (!process.HasExited) process.Kill(entireProcessTree: true);
                await process.WaitForExitAsync();
            }
        }
        finally { directory.Delete(true); }
    }

    [Theory]
    [InlineData("10.0.112", "10.0.100")]
    [InlineData("10.0.400", "10.0.400")]
    [InlineData("10.0.401", "10.0.400")]
    public void Servicing_releases_share_their_sdk_feature_bands(string version, string expected)
    {
        Assert.True(WorkloadInstallation.FeatureBand(version) == expected,
            "workload manifests must be registered in the SDK's feature band, not its servicing version");
    }

    [Theory]
    [InlineData("9.0.400")]
    [InlineData("10.0")]
    [InlineData("unavailable")]
    public void An_unsupported_sdk_gets_an_actionable_error(string version)
    {
        var error = Assert.Throws<InvalidOperationException>(() => WorkloadInstallation.FeatureBand(version));
        Assert.True(error.Message.Contains("Install a .NET 10 SDK", StringComparison.Ordinal),
            "the SDK check must explain how to unblock installation");
    }
}
