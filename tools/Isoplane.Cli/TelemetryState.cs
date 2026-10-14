using System.Text.Json;

namespace Isoplane.Cli;

/// <summary>What a user has chosen about telemetry and the identity their events carry.</summary>
internal sealed record TelemetryState(bool Enabled = true, Guid? InstallId = null, bool NoticeShown = false);

internal sealed class TelemetryStateFile(string directory)
{
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web) { WriteIndented = true };

    internal string Pathname => Path.Combine(directory, "state.json");

    /// <summary>
    /// The saved state, the defaults when none has been saved, or null when the file is there and cannot be
    /// read: a choice to opt out may be in it, so nothing is recorded until it is replaced.
    /// </summary>
    internal TelemetryState? Load()
    {
        try { return JsonSerializer.Deserialize<TelemetryState>(File.ReadAllText(Pathname), Json); }
        catch (FileNotFoundException) { return new(); }
        catch (DirectoryNotFoundException) { return new(); }
        catch (Exception error) when (error is JsonException or IOException or UnauthorizedAccessException) { return null; }
    }

    internal void Save(TelemetryState state)
    {
        Directory.CreateDirectory(directory);
        var temporary = Path.Combine(directory, ".state-" + Guid.NewGuid().ToString("N") + ".tmp");
        try
        {
            File.WriteAllText(temporary, JsonSerializer.Serialize(state, Json));
            File.Move(temporary, Pathname, overwrite: true);
        }
        finally { File.Delete(temporary); }
    }
}

internal enum TelemetryMode { Enabled, Unavailable, OptedOutByVariable, OptedOutByCommand, StateUnreadable }

/// <summary>Whether this run may record and send, and what decided it.</summary>
internal sealed record TelemetryStatus(TelemetryMode Mode, string? Variable = null)
{
    internal static TelemetryStatus Resolve(Uri? endpoint, Func<string, string?> environment, TelemetryState? state)
    {
        if (endpoint is null) return new(TelemetryMode.Unavailable);
        if (TelemetryVariables.OptOuts.FirstOrDefault(candidate => TelemetryVariables.IsOn(environment(candidate.Name))) is { } variable)
            return new(TelemetryMode.OptedOutByVariable, variable.Name);
        if (state is null) return new(TelemetryMode.StateUnreadable);
        return new(state.Enabled ? TelemetryMode.Enabled : TelemetryMode.OptedOutByCommand);
    }
}
