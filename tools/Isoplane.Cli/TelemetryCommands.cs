namespace Isoplane.Cli;

internal static class TelemetryCommands
{
    /// <summary><c>isoplane telemetry status</c>.</summary>
    internal static int Status(TelemetryHost host, TextWriter output)
    {
        output.WriteLine(Describe(host));
        return 0;
    }

    /// <summary><c>isoplane telemetry disable</c>.</summary>
    internal static int Disable(TelemetryHost host, TextWriter output)
    {
        // The identity goes too, so enabling again later starts as a new installation.
        host.State.Save(new(Enabled: false, InstallId: null, NoticeShown: true));
        host.Outbox.Clear();
        output.WriteLine("Telemetry is disabled. Events that had not been sent were deleted.");
        return 0;
    }

    /// <summary><c>isoplane telemetry enable</c>.</summary>
    internal static int Enable(TelemetryHost host, TextWriter output)
    {
        host.State.Save((host.State.Load() ?? new()) with { Enabled = true, NoticeShown = true });
        output.WriteLine(Describe(host));
        return 0;
    }

    private static string Describe(TelemetryHost host)
    {
        var state = host.State.Load();
        var status = TelemetryStatus.Resolve(host.Endpoint, host.Environment, state);
        return status.Mode switch
        {
            TelemetryMode.Unavailable => "This build of the CLI does not collect telemetry.",
            TelemetryMode.OptedOutByVariable => $"Telemetry is disabled by {status.Variable}.",
            TelemetryMode.OptedOutByCommand => "Telemetry is disabled. Run isoplane telemetry enable to turn it on.",
            TelemetryMode.StateUnreadable => $"Telemetry is off because {host.State.Pathname} cannot be read. Run isoplane telemetry enable or disable to replace it.",
            TelemetryMode.Enabled => "Telemetry is enabled. " + (state?.InstallId is { } id
                ? $"Installation identifier: {id:D}" : "This installation has no identifier yet."),
            // C# requires an arm for numbers that name no mode.
            _ => throw new System.Diagnostics.UnreachableException(),
        };
    }
}
