using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class SdkRegistrationsTests
{
    private const string Sources = """
        Registered Sources:
          1.  nuget.org [Enabled]
              https://api.nuget.org/v3/index.json
          2.  isoplane [Enabled]
              /home/dev/.isoplane/packages
          3.  corp [Disabled]
              https://nuget.corp.example/v3/index.json

        """;

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
    public void A_source_is_found_by_name_with_its_path()
    {
        Assert.Equal("/home/dev/.isoplane/packages", DotnetRegistrations.SourcePath(Sources, "isoplane"));
        Assert.Equal("https://nuget.corp.example/v3/index.json", DotnetRegistrations.SourcePath(Sources, "corp"));
        Assert.Null(DotnetRegistrations.SourcePath(Sources, "missing"));
        Assert.Null(DotnetRegistrations.SourcePath("Registered Sources:\n", "isoplane"));
    }

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
        Assert.Equal("dotnet", Path.GetFileName(dotnet));
    }
}
