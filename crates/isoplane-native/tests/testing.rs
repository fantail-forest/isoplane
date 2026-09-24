//! An ordinary test project consumes the local testing package and installed CLI.

use std::path::Path;
use std::process::Command;

#[test]
fn a_dotnet_test_project_runs_its_application_in_the_installed_sandbox() {
    let directory = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(directory.path());
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for package in ["Isoplane.Native", "Isoplane.Testing"] {
        run(Command::new("dotnet")
            .arg("pack")
            .arg(root.join(format!("tools/{package}/{package}.csproj")))
            .arg("--artifacts-path")
            .arg(directory.path().join("testing-artifacts"))
            .arg("-o")
            .arg(directory.path().join("packages"))
            .arg("--disable-build-servers"));
    }
    let consumer = directory.path().join("consumer");
    std::fs::create_dir(&consumer).unwrap();
    std::fs::copy(app.join("nuget.config"), consumer.join("nuget.config")).unwrap();
    std::fs::write(
        consumer.join("Consumer.csproj"),
        format!(
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <ImplicitUsings>enable</ImplicitUsings>
    <Nullable>enable</Nullable>
    <IsTestProject>true</IsTestProject>
    <TreatWarningsAsErrors>true</TreatWarningsAsErrors>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Isoplane.Testing" Version="{}" />
    <PackageReference Include="Microsoft.NET.Test.Sdk" Version="17.14.1" />
    <PackageReference Include="xunit" Version="2.9.3" />
    <PackageReference Include="xunit.runner.visualstudio" Version="3.1.1" PrivateAssets="all" />
  </ItemGroup>
</Project>"#,
            isoplane_fixtures::release_version()
        ),
    )
    .unwrap();
    std::fs::write(
        consumer.join("ApplicationTests.cs"),
        r#"using Isoplane.Testing;
using Xunit;

public sealed class ApplicationTests
{
    [Theory]
    [InlineData("IsoplaneInterpretedDebug")]
    public async Task the_application_runs_inside_the_installed_sandbox_and_releases_its_listener(string configuration)
    {
        using var startup = new CancellationTokenSource(TimeSpan.FromMinutes(20));
        var app = await IsoplaneApplication.StartAsync(
            Environment.GetEnvironmentVariable("TEST_APP")!,
            new ApplicationOptions { CliPath = Environment.GetEnvironmentVariable("TEST_CLI")!, Configuration = configuration },
            startup.Token);
        var address = app.Client.BaseAddress!;
        try
        {
            using var response = await app.Client.GetAsync("/", startup.Token);
            var body = await response.Content.ReadAsStringAsync(startup.Token);
            Assert.True(response.IsSuccessStatusCode, $"The sandbox must serve the real customer application: {body}\n{app.DiagnosticOutput}");
            Assert.True(body.Contains("Isoplane", StringComparison.Ordinal), $"The response must come from the generated application: {body}");
        }
        finally
        {
            await app.DisposeAsync();
        }
        using var client = new HttpClient();
        await Assert.ThrowsAsync<HttpRequestException>(() => client.GetAsync(address, startup.Token));
    }
}
"#,
    )
    .unwrap();
    run(Command::new("dotnet")
        .args([
            "test",
            "--disable-build-servers",
            "--logger",
            "console;verbosity=normal",
        ])
        .env("TEST_APP", &app)
        .env("TEST_CLI", &cli)
        .env(
            "ISOPLANE_GUEST_RUNTIME_FEED",
            isoplane_fixtures::guest_runtime_feed(),
        )
        .current_dir(consumer));
}

fn run(command: &mut Command) {
    let output = command
        .output()
        .expect("dotnet SDK must be installed for the packaged testing consumer");
    assert!(
        output.status.success(),
        "the installed testing-package consumer must succeed ({command:?}):\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
