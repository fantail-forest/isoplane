using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class PrerequisitesTests
{
    [Fact]
    public void Every_prerequisite_present_leaves_nothing_missing()
    {
        Assert.Empty(Prerequisites.Missing(_ => true, () => true));
    }

    [Fact]
    public void Any_icu_version_the_sdk_probes_satisfies_the_icu_prerequisite()
    {
        Assert.DoesNotContain(Prerequisites.Missing(soname => soname == "libicuuc.so.76", () => true), library => library.Name == "ICU");
        Assert.Contains(Prerequisites.Missing(soname => !soname.StartsWith("libicuuc", StringComparison.Ordinal), () => true), library => library.Name == "ICU");
    }

    [Fact]
    public void A_machine_with_no_trusted_roots_lacks_the_certificate_prerequisite()
    {
        var missing = Prerequisites.Missing(_ => true, () => false);
        Assert.Equal("ca-certificates", Assert.Single(missing).UbuntuPackage);
        Assert.Equal("CA certificates (none in the machine's certificate store, for downloads and TLS connections)", Prerequisites.Describe(missing));
    }

    [Fact]
    public void The_install_command_names_the_ubuntu_packages_only_on_ubuntu_24_04()
    {
        var missing = Prerequisites.Missing(soname => soname is not ("libz.so.1" or "libssl.so.3"), () => true);
        Assert.Equal("sudo apt-get install -y libssl3t64 zlib1g", Prerequisites.InstallCommand(new LinuxDistribution("ubuntu", "24.04"), missing));
        Assert.Null(Prerequisites.InstallCommand(new LinuxDistribution("ubuntu", "22.04"), missing));
        Assert.Null(Prerequisites.InstallCommand(new LinuxDistribution("debian", "12"), missing));
        Assert.Null(Prerequisites.InstallCommand(LinuxDistribution.Unknown, missing));
    }
}
