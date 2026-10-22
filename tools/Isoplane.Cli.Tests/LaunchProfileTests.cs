using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class LaunchProfileTests : IDisposable
{
    private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("isoplane-launch-profile-");

    private void Settings(string json)
    {
        Directory.CreateDirectory(Path.Combine(directory.FullName, "Properties"));
        File.WriteAllText(Path.Combine(directory.FullName, "Properties", "launchSettings.json"), json);
    }

    private const string TwoProfiles = """
        {
          // Comments and trailing commas are valid in launch settings.
          "profiles": {
            "IIS Express": { "commandName": "IISExpress", "applicationUrl": "http://localhost:1" },
            "https": {
              "commandName": "Project",
              "applicationUrl": "https://localhost:7090;http://localhost:5197",
              "environmentVariables": { "ASPNETCORE_ENVIRONMENT": "Development", "ConnectionStrings__Todos": "Host=db" },
            },
            "tight": {
              "commandName": "Project",
              "isoplaneLimitsOverride": { "memoryBytes": 67108864, "occupancyMsPerDispatch": 100, "concurrentConnectionsPerInstance": 20, "pendingFuturesPerDispatch": 6 }
            }
          }
        }
        """;

    [Fact]
    public void a_project_without_launch_settings_runs_without_a_profile()
    {
        Assert.Null(LaunchProfile.Read(directory.FullName, null));
    }

    [Fact]
    public void the_first_profile_that_launches_the_project_supplies_its_http_port_and_environment()
    {
        Settings(TwoProfiles);
        var profile = LaunchProfile.Read(directory.FullName, null)!;
        Assert.Equal("https", profile.Name);
        Assert.Equal((ushort)5197, profile.Port);
        Assert.Equal("Development", profile.Environment["ASPNETCORE_ENVIRONMENT"]);
        Assert.Equal("Host=db", profile.Environment["ConnectionStrings__Todos"]);
        Assert.Equal(LimitOverrides.None.MemoryBytes, profile.Limits.MemoryBytes);
        Assert.Empty(profile.Limits.ResourceLimits);
    }

    [Fact]
    public void a_selected_profile_narrows_limits_by_their_manifest_names()
    {
        Settings(TwoProfiles);
        var profile = LaunchProfile.Read(directory.FullName, "tight")!;
        Assert.Null(profile.Port);
        Assert.Equal(67108864UL, profile.Limits.MemoryBytes);
        Assert.Equal(new Dictionary<string, ulong>
        {
            ["occupancy_ms_per_dispatch"] = 100, ["instance_open_connections"] = 20, ["pending_futures_per_dispatch"] = 6
        }, profile.Limits.ResourceLimits);
    }

    [Theory]
    [InlineData("missing", "no launch profile named 'missing'")]
    [InlineData("IIS Express", "does not launch the project")]
    public void a_selected_profile_that_cannot_run_the_project_is_refused(string selected, string reason)
    {
        Settings(TwoProfiles);
        var failure = Assert.Throws<ArgumentException>(() => LaunchProfile.Read(directory.FullName, selected));
        Assert.Contains(reason, failure.Message, StringComparison.Ordinal);
    }

    [Theory]
    [InlineData("""{ "unsupported": 1 }""", "does not support 'unsupported'")]
    [InlineData("""{ "subrequestsPerDispatch": 20 }""", "does not support 'subrequestsPerDispatch'")]
    [InlineData("""{ "concurrentConnectionsPerInstance": 0 }""", "must be a positive whole number")]
    [InlineData("""{ "memoryBytes": "64MB" }""", "must be a positive whole number")]
    [InlineData("""{ "memoryBytes": 0 }""", "must be a positive whole number")]
    [InlineData("\"invalid\"", "must be an object")]
    public void an_unsupported_limit_override_is_refused_by_name(string overrides, string reason)
    {
        Settings($$"""{ "profiles": { "http": { "commandName": "Project", "isoplaneLimitsOverride": {{overrides}} } } }""");
        var failure = Assert.Throws<ArgumentException>(() => LaunchProfile.Read(directory.FullName, null));
        Assert.Contains(reason, failure.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void malformed_launch_settings_name_their_file()
    {
        Settings("{ \"profiles\": ");
        var failure = Assert.Throws<ArgumentException>(() => LaunchProfile.Read(directory.FullName, null));
        Assert.Contains("launchSettings.json", failure.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void session_options_apply_numeric_budgets_and_profile_overrides()
    {
        Settings(TwoProfiles);
        var settings = new DevelopmentSettings("App", 0, [], LaunchProfile.Read(directory.FullName, "tight")!.Limits, null,
            Path.Combine(directory.FullName, "obj"), Path.Combine(directory.FullName, "runtimes"));
        using var options = System.Text.Json.JsonDocument.Parse(new LocalProject(directory.FullName).SessionOptions(settings));
        var manifest = options.RootElement.GetProperty("manifest");
        Assert.Equal("App", manifest.GetProperty("name").GetString());
        Assert.Equal(536870912UL, manifest.GetProperty("limits").GetProperty("instance_memory_max").GetUInt64());
        Assert.Equal(67108864UL, manifest.GetProperty("guest_memory_bytes").GetUInt64());
        Assert.Equal(100UL, manifest.GetProperty("resource_limits").GetProperty("occupancy_ms_per_dispatch").GetUInt64());
        Assert.False(options.RootElement.TryGetProperty("machine_roots", out _), "A session without exported roots must leave the platform bundle selected.");
    }

    [Fact]
    public void settings_layer_user_secrets_then_the_profile_then_prefixed_process_variables()
    {
        Settings(TwoProfiles);
        var secrets = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            ["ConnectionStrings:Todos"] = "Host=secret", ["Only:Secret"] = "kept"
        };
        var process = new System.Collections.Hashtable
        {
            ["ISOPLANE_APP_ASPNETCORE_ENVIRONMENT"] = "Staging", ["ISOPLANE_APP_Feature__Enabled"] = "true",
            ["ConnectionStrings__Todos"] = "Host=unprefixed", ["ISOPLANE_APP_"] = "ignored"
        };
        var settings = DevelopmentSettings.Layer(secrets, LaunchProfile.Read(directory.FullName, null), process);
        Assert.Equal("Host=db", settings["ConnectionStrings:Todos"]);
        Assert.Equal("kept", settings["Only:Secret"]);
        Assert.Equal("Staging", settings["ASPNETCORE_ENVIRONMENT"]);
        Assert.Equal("true", settings["Feature:Enabled"]);
        Assert.Equal(4, settings.Count);
    }

    public void Dispose() => directory.Delete(recursive: true);
}
