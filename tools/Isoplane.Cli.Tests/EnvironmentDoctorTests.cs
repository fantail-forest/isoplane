using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class EnvironmentDoctorTests
{
    [Theory]
    [InlineData(null)]
    [InlineData("0.1.0-preview.1")]
    public void Missing_or_mismatched_templates_require_setup(string? installed)
    {
        var error = Assert.Throws<InvalidOperationException>(() => EnvironmentDoctor.RequireTemplates(installed, "0.1.0-preview.4"));
        Assert.Contains("isoplane setup", error.Message);
        EnvironmentDoctor.RequireTemplates("0.1.0-preview.4", "0.1.0-preview.4");
    }

    [Theory]
    [InlineData("9.0.100")]
    [InlineData("not a version")]
    public void An_unsupported_sdk_is_rejected(string version)
    {
        Assert.Throws<InvalidOperationException>(() => Readiness.RequireSdkVersion(version));
        Readiness.RequireSdkVersion("10.0.100");
    }
}
