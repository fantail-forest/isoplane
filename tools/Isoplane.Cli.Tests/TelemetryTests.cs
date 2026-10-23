using System.Net;
using System.Text.Json;
using System.Text.RegularExpressions;
using Isoplane.Cli;
using Isoplane.TestSupport;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class TelemetryTests : IDisposable
{
    private static readonly Uri Endpoint = new("https://telemetry.example.test/v1/events");
    private readonly FileSystemTestDirectory home = new("isoplane-telemetry-");
    private readonly Service service = new();
    private readonly StringWriter error = new();
    private readonly Dictionary<string, string> environment = [];

    public void Dispose()
    {
        service.Dispose();
        home.Dispose();
    }

    [Fact]
    public async Task The_first_run_shows_the_notice_and_sends_nothing_and_the_next_run_sends_its_events()
    {
        service.Answer = HttpStatusCode.Accepted;
        var first = CommandTelemetry.Begin(["setup"], Host());
        first.Ended(0);
        await first.Upload;
        Assert.Contains(CommandTelemetry.Notice, error.ToString());
        Assert.Empty(service.Requests);
        var identifier = Host().State.Load()!.InstallId;
        Assert.Equal(["command", "installation"], Pending().Select(Kind).Order());

        var second = CommandTelemetry.Begin(["dev"], Host());
        await second.Upload;
        second.Ended(0);
        var sent = JsonDocument.Parse(Assert.Single(service.Requests)).RootElement.GetProperty("events").EnumerateArray().ToArray();
        Assert.Equal(["command", "installation"], sent.Select(sentEvent => sentEvent.GetProperty("event").GetString()).Order());
        Assert.All(sent, sentEvent => Assert.Equal(identifier?.ToString("D"), sentEvent.GetProperty("installId").GetString()));
        Assert.Equal(1, Regex.Count(error.ToString(), Regex.Escape(CommandTelemetry.Notice)));
        Assert.Equal("dev", Command(Assert.Single(Pending())));
    }

    [Fact]
    public async Task Events_recorded_while_an_upload_starts_wait_for_the_next_command()
    {
        Seen();
        var earlier = Guid.NewGuid();
        var current = Guid.NewGuid();
        Queue(earlier);
        service.Answer = HttpStatusCode.Accepted;
        var run = CommandTelemetry.Begin(["--version"], Host() with
        {
            CreateClient = () =>
            {
                Queue(current);
                return new HttpClient(service, disposeHandler: false);
            }
        });
        await run.Upload;
        run.Ended(0);
        var sent = JsonDocument.Parse(Assert.Single(service.Requests)).RootElement.GetProperty("events").EnumerateArray().ToArray();
        Assert.Equal(earlier, Assert.Single(sent).GetProperty("id").GetGuid());
        Assert.Equal(current + ".json", Assert.Single(Pending()));
    }

    [Theory]
    [InlineData("complete", "isoplane bundle --out")]
    [InlineData("completions", "bash")]
    public async Task Completion_neither_reads_telemetry_context_nor_sends_queued_events(string command, string argument)
    {
        Seen();
        Queue(Guid.NewGuid());
        var reads = 0;
        var host = Host() with { Environment = _ => { reads++; return null; } };
        var run = CommandTelemetry.Begin([command, argument], host);
        run.Ended(0);
        await run.Upload;
        Assert.Equal(0, reads);
        Assert.Empty(service.Requests);
        Assert.Empty(error.ToString());
        Assert.Single(Pending());
    }

    [Theory]
    [InlineData("ISOPLANE_TELEMETRY_OPTOUT", "1")]
    [InlineData("ISOPLANE_TELEMETRY_OPTOUT", "true")]
    [InlineData("DO_NOT_TRACK", "1")]
    public async Task An_opt_out_variable_stops_the_notice_recording_and_sending(string variable, string value)
    {
        Seen();
        Queue(Guid.NewGuid());
        environment[variable] = value;
        var run = CommandTelemetry.Begin(["dev"], Host());
        await run.Upload;
        run.Ended(0);
        Assert.Empty(error.ToString());
        Assert.Empty(service.Requests);
        Assert.Single(Pending());
        Assert.Equal($"Telemetry is disabled by {variable}.", Status());
    }

    [Fact]
    public async Task Disabling_deletes_unsent_events_and_the_installation_identifier_until_enabled_again()
    {
        Seen();
        CommandTelemetry.Begin(["dev"], Host()).Ended(0);
        var identifier = Host().State.Load()!.InstallId;
        Assert.NotNull(identifier);
        Assert.Equal($"Telemetry is enabled. Installation identifier: {identifier:D}", Status());

        service.Answer = HttpStatusCode.Accepted;
        var clients = 0;
        var contextReads = 0;
        var host = Host() with
        {
            Environment = name => { contextReads++; return environment.GetValueOrDefault(name); },
            CreateClient = () => { clients++; return new HttpClient(service, disposeHandler: false); }
        };
        string[] args = ["telemetry", "disable"];
        Assert.Empty(CliCommandLine.Parse(args).Errors);
        var disabling = CommandTelemetry.Begin(args, host);
        await disabling.Upload.WaitAsync(TimeSpan.FromSeconds(30));
        Assert.Equal(0, clients);
        Assert.Equal(0, contextReads);
        Assert.Equal(0, disabling.Ended(TelemetryCommands.Run(args, host, TextWriter.Null)));
        await disabling.Upload;
        Assert.Equal(0, clients);
        Assert.Empty(service.Requests);
        Assert.Empty(Pending());
        Assert.False(Host().State.Load()!.Enabled);
        Assert.Null(Host().State.Load()!.InstallId);
        var disabled = CommandTelemetry.Begin(["dev"], Host());
        await disabled.Upload;
        disabled.Ended(0);
        Assert.Empty(Pending());
        Assert.Empty(service.Requests);

        Assert.Equal(0, TelemetryCommands.Run(["telemetry", "enable"], Host(), TextWriter.Null));
        CommandTelemetry.Begin(["dev"], Host()).Ended(0);
        Assert.NotEqual(identifier, Host().State.Load()!.InstallId);
        Assert.Equal(["command", "installation"], Pending().Select(Kind).Order());
    }

    [Theory]
    [InlineData("status")]
    [InlineData("enable")]
    public async Task Checking_or_enabling_telemetry_preserves_queued_events_without_starting_telemetry(string command)
    {
        Seen();
        var state = Host().State.Load();
        var queued = Path.GetFileName(Queue(Guid.NewGuid()));
        service.Answer = HttpStatusCode.Accepted;
        var clients = 0;
        var contextReads = 0;
        var host = Host() with
        {
            Environment = name => { contextReads++; return environment.GetValueOrDefault(name); },
            CreateClient = () => { clients++; return new HttpClient(service, disposeHandler: false); }
        };
        string[] args = ["telemetry", command];
        Assert.Empty(CliCommandLine.Parse(args).Errors);
        var run = CommandTelemetry.Begin(args, host);
        await run.Upload.WaitAsync(TimeSpan.FromSeconds(30));
        Assert.Equal(0, clients);
        Assert.Equal(0, contextReads);
        var output = new StringWriter();
        Assert.Equal(0, run.Ended(TelemetryCommands.Run(args, host, output)));
        await run.Upload;
        Assert.Equal(0, clients);
        Assert.Empty(service.Requests);
        Assert.Equal(queued, Assert.Single(Pending()));
        Assert.Equal(state, Host().State.Load());
        Assert.Equal($"Telemetry is enabled. Installation identifier: {state!.InstallId:D}", output.ToString().TrimEnd());
        Assert.Empty(error.ToString());
    }

    [Fact]
    public async Task Debug_mode_prints_the_event_and_neither_stores_nor_sends_it()
    {
        Seen();
        Queue(Guid.NewGuid());
        environment["ISOPLANE_TELEMETRY_DEBUG"] = "1";
        var run = CommandTelemetry.Begin(["bundle", "--output", "secret-name"], Host());
        await run.Upload;
        run.Failed(new ArgumentException("secret-message"), 1);
        var printed = JsonDocument.Parse(error.ToString()[error.ToString().IndexOf('{')..]).RootElement;
        Assert.Equal("bundle", printed.GetProperty("command").GetString());
        Assert.Equal("usage", printed.GetProperty("failure").GetString());
        Assert.DoesNotContain("secret", error.ToString());
        Assert.Single(Pending());
        Assert.Empty(service.Requests);
    }

    [Fact]
    public async Task A_build_without_an_endpoint_never_shows_the_notice_or_records()
    {
        var run = CommandTelemetry.Begin(["dev"], Host() with { Endpoint = null });
        await run.Upload;
        run.Ended(0);
        Assert.Empty(error.ToString());
        Assert.False(Directory.Exists(Path.Combine(home.Path, "pending")));
        Assert.Equal("This build of the CLI does not collect telemetry.", Status(Host() with { Endpoint = null }));
    }

    [Fact]
    public void A_run_whose_error_output_nobody_reads_records_nothing_until_the_notice_has_been_shown()
    {
        CommandTelemetry.Begin(["dev"], Host() with { ErrorRedirected = true }).Ended(0);
        Assert.Empty(error.ToString());
        Assert.False(Directory.Exists(Path.Combine(home.Path, "pending")));

        CommandTelemetry.Begin(["dev"], Host()).Ended(0);
        CommandTelemetry.Begin(["dev"], Host() with { ErrorRedirected = true }).Ended(0);
        Assert.Equal(["command", "command", "installation"], Pending().Select(Kind).Order());
    }

    [Fact]
    public void A_continuous_integration_run_shows_the_notice_in_its_log_and_carries_no_installation_identifier()
    {
        environment["GITHUB_ACTIONS"] = "true";
        CommandTelemetry.Begin(["bundle"], Host() with { ErrorRedirected = true }).Ended(0);
        Assert.Contains(CommandTelemetry.Notice, error.ToString());
        var recorded = JsonDocument.Parse(File.ReadAllText(Path.Combine(home.Path, "pending", Assert.Single(Pending())))).RootElement;
        Assert.True(recorded.GetProperty("ci").GetBoolean());
        Assert.Equal("github-actions", recorded.GetProperty("ciProvider").GetString());
        Assert.Equal(JsonValueKind.Null, recorded.GetProperty("installId").ValueKind);
        Assert.Null(Host().State.Load()!.InstallId);
    }

    [Theory]
    [InlineData(HttpStatusCode.Accepted, 0)]
    [InlineData(HttpStatusCode.BadRequest, 0)]
    [InlineData(HttpStatusCode.ServiceUnavailable, 1)]
    [InlineData(HttpStatusCode.NotFound, 1)]
    public async Task Events_stay_queued_unless_the_service_took_them_or_refused_them_for_good(HttpStatusCode answer, int remaining)
    {
        Seen();
        Queue(Guid.NewGuid());
        service.Answer = answer;
        var run = CommandTelemetry.Begin(["--version"], Host());
        await run.Upload;
        run.Ended(0);
        Assert.Single(service.Requests);
        Assert.Equal(remaining, Pending().Length);
    }

    [Fact]
    public async Task An_upload_still_under_way_when_the_command_ends_is_left_for_a_later_command_after_the_grace_period()
    {
        Seen();
        Queue(Guid.NewGuid());
        service.Hold = cancellation => Task.Delay(Timeout.Infinite, cancellation);
        var run = CommandTelemetry.Begin(["--version"], Host() with { UploadGrace = TimeSpan.Zero });
        await service.Started.Task.WaitAsync(TimeSpan.FromSeconds(30));
        Assert.Equal(0, run.Ended(0));
        await run.Upload.WaitAsync(TimeSpan.FromSeconds(30));
        Assert.True(service.Cancelled, "The upload must be abandoned once the grace period is over.");
        Assert.Single(Pending());
    }

    [Fact]
    public async Task Events_too_old_to_be_useful_and_damaged_ones_are_removed_without_being_sent()
    {
        Seen();
        var old = Queue(Guid.NewGuid());
        File.SetLastWriteTimeUtc(old, DateTime.UtcNow - TelemetryOutbox.LongestWait - TimeSpan.FromDays(1));
        File.WriteAllText(Path.Combine(home.Path, "pending", Guid.NewGuid() + ".json"), "{");
        var run = CommandTelemetry.Begin(["--version"], Host());
        await run.Upload;
        Assert.Empty(service.Requests);
        Assert.Empty(Pending());
    }

    [Fact]
    public async Task An_unreadable_state_file_turns_telemetry_off_until_it_is_replaced()
    {
        Directory.CreateDirectory(home.Path);
        File.WriteAllText(Host().State.Pathname, "not json");
        Queue(Guid.NewGuid());
        var run = CommandTelemetry.Begin(["dev"], Host());
        await run.Upload;
        run.Ended(0);
        Assert.Empty(service.Requests);
        Assert.Single(Pending());
        Assert.StartsWith("Telemetry is off because", Status());
        TelemetryCommands.Run(["telemetry", "enable"], Host(), TextWriter.Null);
        Assert.StartsWith("Telemetry is enabled.", Status());
    }

    [Fact]
    public void An_error_nothing_handles_is_recorded_as_a_crash_without_being_caught()
    {
        Seen();
        var run = CommandTelemetry.Begin(["dev"], Host());
        Action command = () =>
        {
            try { throw new NotSupportedException("secret-message"); }
            catch (Exception) when (run.Crashed()) { throw; }
        };
        Assert.Throws<NotSupportedException>(command);
        var recorded = File.ReadAllText(Path.Combine(home.Path, "pending", Assert.Single(Pending())));
        Assert.Contains("\"outcome\":\"crashed\",\"exitCode\":null,\"failure\":\"unexpected\"", recorded);
        Assert.DoesNotContain("secret", recorded);
    }

    [Fact]
    public void A_failure_to_start_recording_leaves_the_command_and_its_output_alone()
    {
        var run = CommandTelemetry.Begin(["dev"], Host() with { Environment = _ => throw new InvalidOperationException("broken environment") });
        Assert.Equal(3, run.Ended(3));
        Assert.Empty(error.ToString());
        Assert.Empty(Pending());
    }

    [Fact]
    public async Task A_failure_while_recording_or_sending_leaves_the_command_and_its_output_alone()
    {
        Seen();
        Queue(Guid.NewGuid());
        var run = CommandTelemetry.Begin(["dev"], Host() with { Time = new BrokenClock() });
        await run.Upload.WaitAsync(TimeSpan.FromSeconds(30));
        Assert.Equal(0, run.Ended(0));
        Assert.Equal(1, run.Failed(new InvalidOperationException(), 1));
        Assert.Empty(error.ToString());
        Assert.Single(Pending());
    }

    [Fact]
    public void A_full_queue_stops_recording()
    {
        Seen();
        for (var index = 0; index < TelemetryOutbox.Capacity; index++) Queue(Guid.NewGuid());
        CommandTelemetry.Begin(["dev"], Host()).Ended(0);
        Assert.Equal(TelemetryOutbox.Capacity, Pending().Length);
    }

    [Theory]
    [InlineData(null)]
    [InlineData(null, "--help")]
    [InlineData(null, "bundle", "-h")]
    [InlineData(null, "--version")]
    [InlineData(null, "telemetry", "status")]
    [InlineData("doctor", "doctor")]
    [InlineData("doctor-module", "doctor", "/home/someone/app.wasm")]
    [InlineData("bundle", "bundle", "--output", "x")]
    [InlineData("unknown", "depoly")]
    [InlineData("unknown", "doctor-module")]
    public void A_run_is_named_by_its_command_alone(string? expected, params string[] args) =>
        Assert.Equal(expected, CommandNames.Classify(args) is { } command ? CommandNames.Name(command) : null);

    [Fact]
    public void A_distribution_is_reported_as_os_release_states_it_when_it_fits_the_schema()
    {
        Assert.Equal(new LinuxDistribution("ubuntu", "24.04"),
            LinuxDistribution.Parse(["PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"", "ID=ubuntu", "VERSION_ID=\"24.04\"", "HOME_URL=https://ubuntu.com/"]));
        Assert.Equal(new LinuxDistribution("arch", null), LinuxDistribution.Parse(["ID=arch", "BUILD_ID=rolling"]));
        Assert.Equal(new LinuxDistribution("debian", "13~rc1"), LinuxDistribution.Parse(["ID=debian", "VERSION_ID=13~rc1"]));
        Assert.Equal(new LinuxDistribution("linux", null), LinuxDistribution.Parse(["NAME=Custom"]));
        Assert.Equal(new LinuxDistribution(null, null), LinuxDistribution.Parse(["ID=\"Some Company's Linux\"", "VERSION_ID=1 (internal build)"]));
    }

    [Fact]
    public void A_distribution_comes_from_the_second_os_release_file_only_when_the_first_is_missing()
    {
        var usr = Path.Combine(home.Path, "usr-os-release");
        File.WriteAllLines(usr, ["ID=fedora", "VERSION_ID=42"]);
        var missing = Path.Combine(home.Path, "missing-os-release");
        Assert.Equal(new LinuxDistribution("fedora", "42"), LinuxDistribution.Read([missing, usr]));
        var etc = Path.Combine(home.Path, "etc-os-release");
        File.WriteAllLines(etc, ["ID=alpine", "VERSION_ID=3.22.1"]);
        Assert.Equal(new LinuxDistribution("alpine", "3.22.1"), LinuxDistribution.Read([etc, usr]));
        Assert.Equal(new LinuxDistribution(null, null), LinuxDistribution.Read([missing]));
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
    public void An_agent_rule_names_its_agent_as_the_event_does() =>
        Assert.All(TelemetryVariables.Agents, rule => Assert.Equal(CommandNames.Name(rule.Agent), rule.Name));

    [Fact]
    public void The_first_matching_agent_rule_decides()
    {
        environment["PI_CODING_AGENT"] = "true";
        environment["CLAUDECODE"] = "1";
        Assert.Equal(CodingAgent.Claude, TelemetryContext.Read(name => environment.GetValueOrDefault(name), "0.1.0").Agent);
    }

    [Theory]
    [InlineData("run", false, "dev")]
    [InlineData("debug", false, "dev", "--debug")]
    [InlineData("debug", false, "debug-adapter")]
    [InlineData("run", false, "bundle", "--output", "--debug")]
    [InlineData("test", true, "dev", "--json")]
    public void A_run_records_whether_a_test_or_a_debugger_started_it(string expected, bool startedByTest, params string[] args)
    {
        if (startedByTest) environment[TelemetryVariables.TestRun.Name] = "1";
        Seen();
        CommandTelemetry.Begin(args, Host()).Ended(0);
        var recorded = JsonDocument.Parse(File.ReadAllText(Path.Combine(home.Path, "pending", Assert.Single(Pending())))).RootElement;
        Assert.Equal(expected, recorded.GetProperty("mode").GetString());
    }

    [Fact]
    public void Every_command_the_parser_accepts_has_a_telemetry_name_or_is_excluded()
    {
        var commands = CliCommandLine.Create().Subcommands.Select(command => command.Name).ToArray();
        Assert.Contains("bundle", commands);
        Assert.All(commands, command => Assert.NotEqual(TelemetryCommand.Unknown, CommandNames.Classify([command])));
    }

    [Fact]
    public void A_recorded_installation_carries_exactly_what_its_schema_allows()
    {
        var schema = JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "TelemetryProtocol", "installation-event.schema.json"))).RootElement;
        CommandTelemetry.Begin(["dev"], Host()).Ended(0);
        var file = Assert.Single(Pending(), file => Kind(file) == "installation");
        var recorded = JsonDocument.Parse(File.ReadAllText(Path.Combine(home.Path, "pending", file))).RootElement;

        Assert.Equal(schema.GetProperty("properties").EnumerateObject().Select(property => property.Name).Order(),
            recorded.EnumerateObject().Select(property => property.Name).Order());
        foreach (var property in schema.GetProperty("properties").EnumerateObject().Where(property => property.Value.TryGetProperty("pattern", out _)))
            Assert.Matches(property.Value.GetProperty("pattern").GetString()!, recorded.GetProperty(property.Name).GetString()!);
        Assert.Equal(Host().State.Load()!.InstallId?.ToString("D"), recorded.GetProperty("installId").GetString());
    }

    [Fact]
    public void A_recorded_event_carries_exactly_what_the_event_schema_allows()
    {
        var schema = JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "TelemetryProtocol", "command-event.schema.json"))).RootElement;
        var properties = schema.GetProperty("properties");
        Seen();
        CommandTelemetry.Begin(["dev"], Host()).Failed(new IOException(), 1);
        var recorded = JsonDocument.Parse(File.ReadAllText(Path.Combine(home.Path, "pending", Assert.Single(Pending())))).RootElement;

        Assert.Equal(properties.EnumerateObject().Select(property => property.Name).Order(), recorded.EnumerateObject().Select(property => property.Name).Order());
        Assert.Equal(properties.EnumerateObject().Select(property => property.Name).Order(), schema.GetProperty("required").EnumerateArray().Select(name => name.GetString()).Order());
        foreach (var property in properties.EnumerateObject().Where(property => property.Value.TryGetProperty("pattern", out _)))
            Assert.Matches(property.Value.GetProperty("pattern").GetString()!, recorded.GetProperty(property.Name).GetString() ?? "en");
        Assert.Equal(Allowed("agent"), Names<CodingAgent>());
        Assert.Equal(Allowed("command"), Names<TelemetryCommand>());
        Assert.Equal(Allowed("outcome"), Names<CommandOutcome>());
        Assert.Equal(Allowed("mode"), Names<CommandMode>());
        Assert.Equal(Allowed("ciProvider"), TelemetryVariables.CiServices.Select(service => (string?)service.Name)
            .Append(TelemetryVariables.OtherCiService).Append(null).Order());
        Assert.Equal(Allowed("failure"), Names<CommandFailure>().Append(null).Order());
        Assert.Equal(CommandEvent.LongestDurationMs, properties.GetProperty("durationMs").GetProperty("maximum").GetInt64());
        Assert.Equal(7, Guid.Parse(recorded.GetProperty("id").GetString()!).Version);

        IEnumerable<string?> Allowed(string name) => properties.GetProperty(name).GetProperty("enum").EnumerateArray().Select(value => value.GetString()).Order();
        static IEnumerable<string?> Names<T>() where T : struct, Enum => Enum.GetValues<T>().Select(value => (string?)CommandNames.Name(value)).Order();
    }

    private TelemetryHost Host() => new(Endpoint, "0.1.0-preview.4", home.Path, name => environment.GetValueOrDefault(name), TimeProvider.System,
        error, ErrorRedirected: false, () => new HttpClient(service, disposeHandler: false), UploadGrace: TimeSpan.FromSeconds(30));

    private void Seen() => Host().State.Save(new(NoticeShown: true, InstallId: Guid.NewGuid()));

    private string Queue(Guid id)
    {
        Host().Outbox.Add(id, new CommandEvent(id, CommandEvent.Timestamp(DateTimeOffset.UtcNow), null, false, null, CodingAgent.None, TelemetryCommand.Dev,
            CommandMode.Run, CommandOutcome.Succeeded, 0, null, 1, "0.1.0", "linux-x64", "x64", "6.8", null, null, "10.0.0", null).ToJson());
        return Path.Combine(home.Path, "pending", id + ".json");
    }

    private string[] Pending() => Directory.Exists(Path.Combine(home.Path, "pending"))
        ? Directory.GetFiles(Path.Combine(home.Path, "pending")).Select(path => Path.GetFileName(path)).ToArray() : [];

    private string Kind(string file) =>
        JsonDocument.Parse(File.ReadAllText(Path.Combine(home.Path, "pending", file))).RootElement.GetProperty("event").GetString()!;

    private string Command(string file) =>
        JsonDocument.Parse(File.ReadAllText(Path.Combine(home.Path, "pending", file))).RootElement.GetProperty("command").GetString()!;

    private string Status(TelemetryHost? host = null)
    {
        var output = new StringWriter();
        TelemetryCommands.Run(["telemetry", "status"], host ?? Host(), output);
        return output.ToString().TrimEnd();
    }

    private sealed class BrokenClock : TimeProvider
    {
        public override DateTimeOffset GetUtcNow() => throw new InvalidOperationException("broken clock");
    }

    private sealed class Service : HttpMessageHandler
    {
        // Refusing by default keeps every event queued, so that a test of what a command records is not
        // changed by an upload that started with it; tests of sending choose another answer.
        internal HttpStatusCode Answer { get; set; } = HttpStatusCode.ServiceUnavailable;
        internal List<string> Requests { get; } = [];
        /// <summary>Holds each request until it completes; a held request is one still under way.</summary>
        internal Func<CancellationToken, Task> Hold { get; set; } = _ => Task.CompletedTask;
        internal TaskCompletionSource Started { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal bool Cancelled { get; private set; }

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellation)
        {
            Requests.Add(await request.Content!.ReadAsStringAsync(cancellation));
            Started.TrySetResult();
            try { await Hold(cancellation); }
            catch (OperationCanceledException)
            {
                Cancelled = true;
                throw;
            }
            return new HttpResponseMessage(Answer);
        }
    }
}
