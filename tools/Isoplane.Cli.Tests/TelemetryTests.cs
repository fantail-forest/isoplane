using System.Diagnostics;
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
    public async Task The_first_run_shows_the_notice_and_sends_nothing_and_the_next_run_sends_its_event()
    {
        var first = CommandTelemetry.Begin(["dev"], Host());
        first.Ended(0);
        await first.Upload;
        Assert.Contains(CommandTelemetry.Notice, error.ToString());
        Assert.Empty(service.Requests);
        var recorded = Assert.Single(Pending());

        var second = CommandTelemetry.Begin(["setup"], Host());
        await second.Upload;
        second.Ended(0);
        var sent = Assert.Single(JsonDocument.Parse(Assert.Single(service.Requests)).RootElement.GetProperty("events").EnumerateArray());
        Assert.Equal(recorded, sent.GetProperty("id").GetString() + ".json");
        Assert.Equal("dev", sent.GetProperty("command").GetString());
        Assert.Equal(1, Regex.Count(error.ToString(), Regex.Escape(CommandTelemetry.Notice)));
        Assert.Equal("setup", Command(Assert.Single(Pending())));
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

        Assert.Equal(0, TelemetryCommands.Run(["telemetry", "disable"], Host(), TextWriter.Null));
        Assert.Empty(Pending());
        Assert.Null(Host().State.Load()!.InstallId);
        var disabled = CommandTelemetry.Begin(["dev"], Host());
        await disabled.Upload;
        disabled.Ended(0);
        Assert.Empty(Pending());
        Assert.Empty(service.Requests);

        Assert.Equal(0, TelemetryCommands.Run(["telemetry", "enable"], Host(), TextWriter.Null));
        CommandTelemetry.Begin(["dev"], Host()).Ended(0);
        Assert.NotEqual(identifier, Host().State.Load()!.InstallId);
        Assert.Single(Pending());
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
        Assert.Equal(2, Pending().Length);
    }

    [Fact]
    public void A_continuous_integration_run_shows_the_notice_in_its_log_and_carries_no_installation_identifier()
    {
        environment["GITHUB_ACTIONS"] = "true";
        CommandTelemetry.Begin(["bundle"], Host() with { ErrorRedirected = true }).Ended(0);
        Assert.Contains(CommandTelemetry.Notice, error.ToString());
        var recorded = JsonDocument.Parse(File.ReadAllText(Path.Combine(home.Path, "pending", Assert.Single(Pending())))).RootElement;
        Assert.True(recorded.GetProperty("ci").GetBoolean());
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
    public async Task A_command_that_ends_before_sending_starts_sends_nothing()
    {
        Seen();
        Queue(Guid.NewGuid());
        var run = CommandTelemetry.Begin(["--version"], Host() with { BeforeUpload = cancellation => Task.Delay(Timeout.Infinite, cancellation) });
        run.Ended(0);
        await run.Upload;
        Assert.Empty(service.Requests);
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
    public async Task Every_command_the_help_describes_has_a_telemetry_name()
    {
        var start = new ProcessStartInfo("dotnet") { UseShellExecute = false, RedirectStandardOutput = true };
        start.ArgumentList.Add(typeof(CliPaths).Assembly.Location);
        start.ArgumentList.Add("--help");
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start the CLI.");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        var help = await process.StandardOutput.ReadToEndAsync(deadline.Token);
        await process.WaitForExitAsync(deadline.Token);
        var commands = Regex.Matches(help, @"^isoplane ([a-z|-]+)", RegexOptions.Multiline)
            .SelectMany(match => match.Groups[1].Value.Split('|')).Distinct().Except(["telemetry"]).ToArray();
        Assert.Contains("bundle", commands);
        Assert.All(commands, command => Assert.NotEqual(TelemetryCommand.Unknown, CommandNames.Classify([command])));
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
        Assert.Equal(Allowed("platform"), Names<TelemetryPlatform>());
        Assert.Equal(Allowed("failure"), Names<CommandFailure>().Append(null).Order());
        Assert.Equal(CommandEvent.LongestDurationMs, properties.GetProperty("durationMs").GetProperty("maximum").GetInt64());

        IEnumerable<string?> Allowed(string name) => properties.GetProperty(name).GetProperty("enum").EnumerateArray().Select(value => value.GetString()).Order();
        static IEnumerable<string?> Names<T>() where T : struct, Enum => Enum.GetValues<T>().Select(value => (string?)CommandNames.Name(value)).Order();
    }

    private TelemetryHost Host() => new(Endpoint, "0.1.0-preview.4", home.Path, name => environment.GetValueOrDefault(name), TimeProvider.System,
        error, ErrorRedirected: false, () => new HttpClient(service, disposeHandler: false), _ => Task.CompletedTask);

    private void Seen() => Host().State.Save(new(NoticeShown: true));

    private string Queue(Guid id)
    {
        Host().Outbox.Add(id, new CommandEvent(id, CommandEvent.Timestamp(DateTimeOffset.UtcNow), null, false, CodingAgent.None, TelemetryCommand.Dev,
            CommandOutcome.Succeeded, 0, null, 1, "0.1.0", TelemetryPlatform.LinuxX64, "6.8", "10.0.0", null).ToJson());
        return Path.Combine(home.Path, "pending", id + ".json");
    }

    private string[] Pending() => Directory.Exists(Path.Combine(home.Path, "pending"))
        ? Directory.GetFiles(Path.Combine(home.Path, "pending")).Select(path => Path.GetFileName(path)).ToArray() : [];

    private string Command(string file) =>
        JsonDocument.Parse(File.ReadAllText(Path.Combine(home.Path, "pending", file))).RootElement.GetProperty("command").GetString()!;

    private string Status(TelemetryHost? host = null)
    {
        var output = new StringWriter();
        TelemetryCommands.Run(["telemetry", "status"], host ?? Host(), output);
        return output.ToString().TrimEnd();
    }

    private sealed class Service : HttpMessageHandler
    {
        internal HttpStatusCode Answer { get; set; } = HttpStatusCode.Accepted;
        internal List<string> Requests { get; } = [];

        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellation)
        {
            Requests.Add(await request.Content!.ReadAsStringAsync(cancellation));
            return new HttpResponseMessage(Answer);
        }
    }
}
