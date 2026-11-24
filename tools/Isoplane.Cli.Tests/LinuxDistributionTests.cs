using Isoplane.TestSupport;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class LinuxDistributionTests
{
    [Fact]
    public void The_identifier_and_version_are_the_unquoted_values_os_release_assigns()
    {
        Assert.Equal(new LinuxDistribution("ubuntu", "24.04"),
            LinuxDistribution.Parse(["PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"", "NAME=\"Ubuntu\"", "VERSION_ID=\"24.04\"", "ID=ubuntu", "ID_LIKE=debian", "HOME_URL=https://ubuntu.com/"]));
        Assert.Equal(new LinuxDistribution("arch", null), LinuxDistribution.Parse(["ID=arch", "BUILD_ID=rolling"]));
        Assert.Equal(new LinuxDistribution("debian", "13~rc1"), LinuxDistribution.Parse(["ID=debian", "VERSION_ID=13~rc1"]));
        Assert.Equal(new LinuxDistribution("Some Company's Linux", "1 (internal build)"),
            LinuxDistribution.Parse(["ID=\"Some Company's Linux\"", "VERSION_ID=1 (internal build)"]));
    }

    [Fact]
    public void A_name_assigned_twice_has_its_last_value_in_either_kind_of_quote() =>
        Assert.Equal(new LinuxDistribution("ubuntu", "24.04"), LinuxDistribution.Parse(["ID=debian", "ID='ubuntu'", "VERSION_ID='24.04'"]));

    [Fact]
    public void A_file_that_names_no_distribution_has_the_specifications_default_identifier() =>
        Assert.Equal(new LinuxDistribution("linux", null), LinuxDistribution.Parse(["NAME=Custom"]));

    [Fact]
    public void A_distribution_comes_from_the_second_os_release_file_only_when_the_first_is_missing()
    {
        using var directory = new FileSystemTestDirectory("isoplane-os-release-");
        var usr = Path.Combine(directory.Path, "usr-os-release");
        File.WriteAllLines(usr, ["ID=fedora", "VERSION_ID=42"]);
        var missing = Path.Combine(directory.Path, "missing-os-release");
        Assert.Equal(new LinuxDistribution("fedora", "42"), LinuxDistribution.Read([missing, usr]));
        Assert.Equal(new LinuxDistribution("fedora", "42"), LinuxDistribution.Read([Path.Combine(missing, "os-release"), usr]));
        var etc = Path.Combine(directory.Path, "etc-os-release");
        File.WriteAllLines(etc, ["ID=alpine", "VERSION_ID=3.22.1"]);
        Assert.Equal(new LinuxDistribution("alpine", "3.22.1"), LinuxDistribution.Read([etc, usr]));
        Assert.Equal(LinuxDistribution.Unknown, LinuxDistribution.Read([missing]));
    }

    [Fact]
    public void An_os_release_file_that_cannot_be_read_leaves_the_distribution_unknown()
    {
        using var directory = new FileSystemTestDirectory("isoplane-os-release-");
        var usr = Path.Combine(directory.Path, "usr-os-release");
        File.WriteAllLines(usr, ["ID=fedora", "VERSION_ID=42"]);
        // Reading a directory as a file is refused on every platform, whoever runs the test.
        var unreadable = Directory.CreateDirectory(Path.Combine(directory.Path, "etc-os-release")).FullName;
        Assert.Equal(LinuxDistribution.Unknown, LinuxDistribution.Read([unreadable, usr]));
    }

    [Fact]
    public void A_machine_has_a_linux_distribution_exactly_when_it_runs_linux() =>
        Assert.Equal(OperatingSystem.IsLinux(), OperatingSystem.LinuxDistribution() is not null);
}
