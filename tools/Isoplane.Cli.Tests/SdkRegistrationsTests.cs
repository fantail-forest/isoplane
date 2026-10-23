using System.Diagnostics;
using System.Xml.Linq;
using Isoplane.TestSupport;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class SdkRegistrationsTests
{
    private const string Templates = """
        Currently installed items:
           Isoplane.Templates
              Version: 0.1.0-preview.1
              Details:
                 Author: Isoplane
              Templates:
                 Isoplane ASP.NET application (isoplane-web) C#
              Uninstall Command:
                 dotnet new uninstall Isoplane.Templates
           Other.Templates
              Version: 2.0.0

        """;

    [Fact]
    public void A_template_package_is_found_by_id_with_its_version()
    {
        Assert.Equal("0.1.0-preview.1", DotnetRegistrations.TemplateVersion(Templates, "Isoplane.Templates"));
        Assert.Equal("2.0.0", DotnetRegistrations.TemplateVersion(Templates, "Other.Templates"));
        Assert.Null(DotnetRegistrations.TemplateVersion(Templates, "Missing.Templates"));
        Assert.Null(DotnetRegistrations.TemplateVersion("Currently installed items:\n(No Items)\n", "Isoplane.Templates"));
    }

    [Fact]
    public void The_bootstrap_dotnet_is_the_one_running_this_process()
    {
        var dotnet = DotnetRegistrations.BootstrapDotnet();
        Assert.True(File.Exists(dotnet), "the bootstrap dotnet must be a real executable");
        Assert.Equal(OperatingSystem.IsWindows() ? "dotnet.exe" : "dotnet", Path.GetFileName(dotnet));
    }

    [Theory]
    [InlineData("sources", "de-DE")]
    [InlineData("templates", "pt-BR")]
    public async Task Sdk_registrations_remain_visible_under_a_localized_sdk(string scenario, string language)
    {
        using var directory = new FileSystemTestDirectory("isoplane-sdk-registrations-");
        var feed = Environment.GetEnvironmentVariable("ISOPLANE_TEST_PACKAGE_FEED")
            ?? throw new InvalidOperationException("Run task managed:template-package and run this test through task exec.");
        var version = ReleaseIdentity.OfRunningCli().Version;
        var package = Path.Combine(feed, $"Isoplane.Templates.{version}.nupkg");
        Assert.True(File.Exists(package), $"Missing {package}. Run task managed:template-package.");
        var start = new ProcessStartInfo(DotnetRegistrations.BootstrapDotnet())
        {
            WorkingDirectory = directory.Path,
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        foreach (var argument in new[] { typeof(OwnedChild).Assembly.Location, "--verify-sdk-registrations", scenario, directory.Path, package, version })
            start.ArgumentList.Add(argument);
        start.Environment["DOTNET_CLI_HOME"] = Path.Combine(directory.Path, "dotnet-home");
        start.Environment["DOTNET_CLI_UI_LANGUAGE"] = language;
        start.Environment["DOTNET_GENERATE_ASPNET_CERTIFICATE"] = "false";
        if (OperatingSystem.IsWindows())
        {
            start.Environment["APPDATA"] = Path.Combine(directory.Path, "appdata");
            start.Environment["LOCALAPPDATA"] = Path.Combine(directory.Path, "localappdata");
        }
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(2));
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Cannot start the SDK registration fixture.");
        var output = process.StandardOutput.ReadToEndAsync();
        var errors = process.StandardError.ReadToEndAsync();
        try
        {
            await process.WaitForExitAsync(deadline.Token);
            Assert.True(process.ExitCode == 0, $"SDK registration scenario '{scenario}' under {language} failed: {await output}{await errors}");
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

    internal static async Task VerifyRegistrationsAsync(string scenario, string directory, string package, string version)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(1));
        var work = Directory.CreateDirectory(Path.Combine(directory, "work")).FullName;
        new XDocument(new XElement("configuration",
            new XElement("packageSources", new XElement("clear"),
                new XElement("add", new XAttribute("key", "isoplane"), new XAttribute("value", "parent-packages")),
                new XElement("add", new XAttribute("key", "disabled"), new XAttribute("value", "https://packages.example/v3/index.json"))),
            new XElement("disabledPackageSources", new XElement("clear"),
                new XElement("add", new XAttribute("key", "disabled"), new XAttribute("value", "true")))))
            .Save(Path.Combine(directory, "NuGet.Config"));
        new XDocument(new XElement("configuration", new XElement("packageSources",
            new XElement("add", new XAttribute("key", "isoplane"), new XAttribute("value", "child-packages")))))
            .Save(Path.Combine(work, "NuGet.Config"));
        var registrations = new DotnetRegistrations(DotnetRegistrations.BootstrapDotnet(), work);
        if (scenario == "sources")
        {
            Assert.Equal(Path.Combine(work, "child-packages"), await registrations.PackageSourceAsync("isoplane", deadline.Token));
            Assert.Equal("https://packages.example/v3/index.json", await registrations.PackageSourceAsync("disabled", deadline.Token));
            Assert.Null(await registrations.PackageSourceAsync("missing", deadline.Token));
            Assert.Null(await registrations.PackageSourceAsync("ISOPLANE", deadline.Token));
            var added = Path.Combine(directory, "added-packages");
            await registrations.AddPackageSourceAsync("added", added, deadline.Token);
            Assert.Equal(added, await registrations.PackageSourceAsync("added", deadline.Token));
            Assert.Contains("added", await File.ReadAllTextAsync(Path.Combine(directory, "NuGet.Config"), deadline.Token));
            await registrations.RemovePackageSourceAsync("added", deadline.Token);
            Assert.Null(await registrations.PackageSourceAsync("added", deadline.Token));

            var home = Directory.CreateDirectory(Path.Combine(directory, "isoplane-home")).FullName;
            Installation.Begin(home, ReleaseIdentity.OfRunningCli()).Record(new(InstalledResourceKind.PackageSource, "isoplane", Owned: true));
            await File.WriteAllTextAsync(Path.Combine(directory, "NuGet.Config"), "<invalid", deadline.Token);
            Assert.Equal(1, await SdkRemoval.RunAsync(home, _ => registrations, deadline.Token));
            var retained = Assert.Single(Installation.Load(home)!.Resources);
            Assert.Equal("isoplane", retained.Target);
            Assert.Contains("Cannot read NuGet package source", retained.RemovalFailure);
            return;
        }
        Assert.Equal("templates", scenario);
        Assert.Null(await registrations.TemplateVersionAsync("Isoplane.Templates", deadline.Token));
        await registrations.InstallTemplateAsync(package, deadline.Token);
        Assert.Equal(version, await registrations.TemplateVersionAsync("Isoplane.Templates", deadline.Token));
        await registrations.UninstallTemplateAsync("Isoplane.Templates", deadline.Token);
        Assert.Null(await registrations.TemplateVersionAsync("Isoplane.Templates", deadline.Token));
    }
}
