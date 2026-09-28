using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class PrerequisitesTests
{
    [Fact]
    public void Every_prerequisite_present_leaves_nothing_missing()
    {
        Assert.Empty(Prerequisites.Missing(_ => true, _ => true));
    }

    [Fact]
    public void Any_icu_version_the_sdk_probes_satisfies_the_icu_prerequisite()
    {
        Assert.DoesNotContain(Prerequisites.Missing(soname => soname == "libicuuc.so.76", _ => true), library => library.Name == "ICU");
        Assert.Contains(Prerequisites.Missing(soname => !soname.StartsWith("libicuuc", StringComparison.Ordinal), _ => true), library => library.Name == "ICU");
    }

    [Fact]
    public void The_certificate_bundle_is_a_file_prerequisite()
    {
        var missing = Prerequisites.Missing(_ => true, path => path != Prerequisites.CertificateBundle);
        Assert.Equal("ca-certificates", Assert.Single(missing).UbuntuPackage);
    }

    [Fact]
    public void The_install_command_names_the_ubuntu_packages_only_on_ubuntu_24_04()
    {
        var missing = Prerequisites.Missing(soname => soname is not ("libz.so.1" or "libssl.so.3"), _ => true);
        Assert.Equal("sudo apt-get install -y libssl3t64 zlib1g", Prerequisites.InstallCommand(new OperatingSystemRelease("ubuntu", "24.04"), missing));
        Assert.Null(Prerequisites.InstallCommand(new OperatingSystemRelease("ubuntu", "22.04"), missing));
        Assert.Null(Prerequisites.InstallCommand(new OperatingSystemRelease("debian", "12"), missing));
    }

    [Fact]
    public void The_distribution_is_read_from_os_release()
    {
        var path = Path.Combine(Directory.CreateTempSubdirectory("isoplane-os-release-").FullName, "os-release");
        try
        {
            File.WriteAllText(path, "PRETTY_NAME=\"Ubuntu 24.04.2 LTS\"\nNAME=\"Ubuntu\"\nVERSION_ID=\"24.04\"\nID=ubuntu\nID_LIKE=debian\n");
            Assert.Equal(new OperatingSystemRelease("ubuntu", "24.04"), Prerequisites.Detect(path));
            Assert.Equal(new OperatingSystemRelease("unknown", ""), Prerequisites.Detect(path + ".missing"));
        }
        finally
        {
            Directory.Delete(Path.GetDirectoryName(path)!, recursive: true);
        }
    }
}
