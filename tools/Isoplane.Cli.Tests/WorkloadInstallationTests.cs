using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class WorkloadInstallationTests
{
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
