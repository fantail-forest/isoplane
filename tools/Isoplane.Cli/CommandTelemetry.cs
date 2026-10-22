using System.Diagnostics;
using System.Reflection;

namespace Isoplane.Cli;

/// <summary>What telemetry reads from the process and the build it runs in.</summary>
internal sealed record TelemetryHost(
    Uri? Endpoint, string CliVersion, string Directory, Func<string, string?> Environment, TimeProvider Time,
    TextWriter Error, bool ErrorRedirected, Func<HttpClient> CreateClient, TimeSpan UploadGrace)
{
    internal const string EndpointMetadata = "IsoplaneTelemetryEndpoint";

    internal TelemetryStateFile State => new(Directory);
    internal TelemetryOutbox Outbox => new(Path.Combine(Directory, "pending"), Time);

    /// <summary>
    /// The running CLI's host. Only a build given <c>-p:IsoplaneTelemetryEndpoint</c> has an endpoint, so a
    /// CLI built from source records and sends nothing.
    /// </summary>
    internal static TelemetryHost OfRunningCli()
    {
        var assembly = typeof(TelemetryHost).Assembly;
        var version = (assembly.GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion ?? "").Split('+')[0];
        var configured = assembly.GetCustomAttributes<AssemblyMetadataAttribute>().FirstOrDefault(entry => entry.Key == EndpointMetadata)?.Value;
        var endpoint = Uri.TryCreate(configured, UriKind.Absolute, out var parsed) && parsed.Scheme is "https" or "http" ? parsed : null;
        return new(endpoint, version, Path.Combine(CliPaths.Home, "telemetry"), System.Environment.GetEnvironmentVariable, TimeProvider.System,
            Console.Error, Console.IsErrorRedirected, () => NewClient(version),
            // Long enough for most uploads still under way to finish, short enough to go unnoticed at the end
            // of a command. What one leaves unsent goes with a later command.
            TimeSpan.FromMilliseconds(250));
    }

    private static HttpClient NewClient(string version)
    {
        var client = new HttpClient(CertificateTrust.CreateHandler(allowAutoRedirect: false)) { Timeout = TimeSpan.FromSeconds(30) };
        client.DefaultRequestHeaders.UserAgent.ParseAdd($"isoplane/{version}");
        return client;
    }
}

/// <summary>
/// Records one command run. Events recorded by earlier runs are sent while this one works, and its own
/// event waits for a later run. A command waits on the network only when it ends with an upload still
/// under way, and then for at most <see cref="TelemetryHost.UploadGrace"/>.
/// </summary>
internal sealed class CommandTelemetry
{
    internal static readonly string Notice =
        "Isoplane collects anonymous usage data to improve the CLI: which commands run, whether they succeed and on what platform.\n" +
        "It never includes project names, paths, arguments or file contents, and this run has sent nothing.\n" +
        $"To opt out, run `isoplane telemetry disable` or set {TelemetryVariables.OptOut.Name}=1.\n" +
        "Details: https://isoplane.dev/docs/reference/telemetry";

    private readonly TelemetryHost? host;
    private readonly TelemetryCommand? command;
    private readonly CommandMode mode;
    private readonly TelemetryContext? context;
    private readonly bool debug;
    private readonly bool recording;
    private readonly long started = Stopwatch.GetTimestamp();
    private readonly CancellationTokenSource uploading = new();

    /// <summary>Completes when this run has stopped sending earlier runs' events.</summary>
    internal Task Upload { get; } = Task.CompletedTask;

    private CommandTelemetry() { }

    private CommandTelemetry(string[] args, TelemetryHost host)
    {
        this.host = host;
        command = CommandNames.Classify(args);
        if (command is { } named) mode = CommandNames.Mode(args, named, host.Environment);
        context = TelemetryContext.Read(host.Environment, host.CliVersion);
        debug = TelemetryVariables.IsOn(host.Environment(TelemetryVariables.Debug.Name));
        var state = host.State.Load();
        if (debug || state is null || host.Endpoint is not { } endpoint
            || TelemetryStatus.Resolve(endpoint, host.Environment, state).Mode != TelemetryMode.Enabled)
            return;
        if (state.NoticeShown)
        {
            recording = true;
            Upload = UploadAsync(host, endpoint, uploading.Token);
        }
        // Where nobody would read the notice, nothing is recorded until a run that can show it.
        else if (args is not ["telemetry", ..] && (!host.ErrorRedirected || context.Ci))
        {
            host.State.Save(state with { NoticeShown = true });
            host.Error.WriteLine(Notice);
            recording = true;
        }
    }

    internal static CommandTelemetry Begin(string[] args) => Begin(args, TelemetryHost.OfRunningCli);

    internal static CommandTelemetry Begin(string[] args, TelemetryHost host) => Begin(args, () => host);

    private static CommandTelemetry Begin(string[] args, Func<TelemetryHost> host)
    {
        try { return new(args, host()); }
        // Telemetry never fails a command or speaks up about itself; a run whose recording cannot start goes unrecorded.
        catch (Exception) { return new(); }
    }

    internal int Ended(int exitCode) =>
        Complete(exitCode == 0 ? CommandOutcome.Succeeded : CommandOutcome.Failed, exitCode, failure: null);

    internal int Cancelled(int exitCode) => Complete(CommandOutcome.Cancelled, exitCode, failure: null);

    internal int Failed(Exception error, int exitCode) => Complete(CommandOutcome.Failed, exitCode, CommandNames.Categorize(error));

    /// <summary>
    /// Records a command ending in an error nothing handles. It returns false so that, used as an exception
    /// filter, it observes the error without catching it.
    /// </summary>
    internal bool Crashed()
    {
        Complete(CommandOutcome.Crashed, exitCode: null, CommandFailure.Unexpected);
        return false;
    }

    private int Complete(CommandOutcome outcome, int? exitCode, CommandFailure? failure)
    {
        if (host is not null && context is not null && command is { } name && (debug || recording))
        {
            try { Record(host, context, name, outcome, exitCode, failure); }
            // Telemetry never fails a command or speaks up about itself; an event that cannot be recorded is lost.
            catch (Exception) { }
        }
        FinishUpload();
        return exitCode ?? 0;
    }

    private void FinishUpload()
    {
        try
        {
            if (host is not null && !Upload.IsCompleted) Upload.Wait(host.UploadGrace);
            uploading.Cancel();
        }
        // An upload's failure is never shown: what it did not send stays queued for a later command.
        catch (Exception) { }
    }

    private void Record(TelemetryHost host, TelemetryContext context, TelemetryCommand name, CommandOutcome outcome, int? exitCode, CommandFailure? failure)
    {
        var now = host.Time.GetUtcNow();
        // Time-ordered, so that the service's index on (occurredAt, id) grows at its end.
        var entry = new CommandEvent(Guid.CreateVersion7(now), CommandEvent.Timestamp(now),
            context.Ci ? null : debug ? host.State.Load()?.InstallId : InstallId(host), context.Ci, context.CiProvider, context.Agent,
            name, mode,
            outcome, exitCode is >= 0 and <= 255 ? exitCode : null, failure,
            Math.Min((long)Stopwatch.GetElapsedTime(started).TotalMilliseconds, CommandEvent.LongestDurationMs),
            context.CliVersion, context.Platform, context.OsArchitecture, context.OsVersion, context.Distribution.Id,
            context.Distribution.Version, context.RuntimeVersion, context.Locale);
        if (debug) host.Error.WriteLine("isoplane telemetry (not sent): " + entry.ToJson());
        else host.Outbox.Add(entry.Id, entry.ToJson());
    }

    private static Guid InstallId(TelemetryHost host)
    {
        var state = host.State.Load() ?? throw new IOException($"Cannot read {host.State.Pathname}.");
        if (state.InstallId is { } known) return known;
        var created = Guid.NewGuid();
        host.State.Save(state with { InstallId = created });
        var now = host.Time.GetUtcNow();
        var registration = new InstallationEvent(Guid.CreateVersion7(now), CommandEvent.Timestamp(now), created);
        host.Outbox.Add(registration.Id, registration.ToJson());
        return created;
    }

    private static async Task UploadAsync(TelemetryHost host, Uri endpoint, CancellationToken cancellation)
    {
        try
        {
            await host.Outbox.SendAsync(host.CreateClient, endpoint, cancellation);
        }
        // Sending is best effort and silent: what the service did not take stays queued for a later run.
        catch (Exception) { }
    }
}
