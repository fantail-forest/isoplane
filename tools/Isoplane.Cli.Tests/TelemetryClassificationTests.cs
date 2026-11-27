using Isoplane.Cli;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class TelemetryClassificationTests
{
    private readonly Dictionary<string, string> environment = [];

    [Theory]
    [InlineData(null)]
    [InlineData(null, "--help")]
    [InlineData(null, "bundle", "-h")]
    [InlineData(null, "--version")]
    [InlineData(null, "telemetry", "status")]
    [InlineData("doctor", "doctor")]
    [InlineData("bundle", "bundle", "--output", "x")]
    [InlineData("unknown", "depoly")]
    public void A_run_is_named_by_its_command_alone(string? expected, params string[] args) =>
        Assert.Equal(expected, CommandNames.Classify(args) is { } command ? CommandNames.Name(command) : null);

    [Theory]
    [InlineData("ubuntu", "24.04", "ubuntu", "24.04")]
    [InlineData("arch", null, "arch", null)]
    [InlineData("debian", "13~rc1", "debian", "13~rc1")]
    [InlineData("Some Company's Linux", "1 (internal build)", null, null)]
    [InlineData("ubuntu", "1 (internal build)", "ubuntu", null)]
    [InlineData(null, null, null, null)]
    public void A_distribution_is_reported_as_os_release_states_it_when_it_fits_the_schema(string? id, string? versionId, string? reportedId, string? reportedVersion)
    {
        var reported = ReportedDistribution.Of(new LinuxDistribution(id, versionId));
        Assert.Equal(reportedId, reported.Id);
        Assert.Equal(reportedVersion, reported.Version);
    }

    [Fact]
    public void No_distribution_is_reported_for_a_machine_that_does_not_run_linux()
    {
        var reported = ReportedDistribution.Of(null);
        Assert.Null(reported.Id);
        Assert.Null(reported.Version);
    }

    [Theory]
    [InlineData("github-actions", "GITHUB_ACTIONS", "true")]
    [InlineData("azure-pipelines", "TF_BUILD", "True")]
    [InlineData("jenkins", "JENKINS_URL", "https://jenkins.example.test/")]
    [InlineData("other", "CI", "woodpecker")]
    [InlineData(null, "CI", "false")]
    [InlineData(null, "GITHUB_ACTIONS", "false")]
    [InlineData(null, "HOME", "/home/someone")]
    public void A_continuous_integration_service_is_named_by_the_variable_it_sets(string? expected, string variable, string value)
    {
        environment[variable] = value;
        Assert.Equal(expected, TelemetryContext.Read(name => environment.GetValueOrDefault(name), "0.1.0").CiProvider);
    }

    [Theory]
    [InlineData("claude", "CLAUDECODE", "1")]
    [InlineData("pi", "PI_CODING_AGENT", "true")]
    [InlineData("copilot-vscode", "AI_AGENT", "GitHub_Copilot_VSCode_Agent")]
    [InlineData("none", "AI_AGENT", "some_other_agent")]
    [InlineData("aider", "OR_APP_NAME", "Aider")]
    [InlineData("none", "OR_APP_NAME", "my-app")]
    [InlineData("other", "AGENT_CLI", "1")]
    [InlineData("none", "CLAUDECODE", "")]
    [InlineData("cursor", "CURSOR_AGENT", "1")]
    [InlineData("none", "CURSOR_EDITOR", "vscode")]
    public void A_coding_agent_is_named_by_the_rules_it_matches(string expected, string variable, string value)
    {
        environment[variable] = value;
        Assert.Equal(expected, CommandNames.Name(TelemetryContext.Read(name => environment.GetValueOrDefault(name), "0.1.0").Agent));
    }

    [Fact]
    public void The_first_matching_agent_rule_decides()
    {
        environment["PI_CODING_AGENT"] = "true";
        environment["CLAUDECODE"] = "1";
        Assert.Equal(CodingAgent.Claude, TelemetryContext.Read(name => environment.GetValueOrDefault(name), "0.1.0").Agent);
    }
}
