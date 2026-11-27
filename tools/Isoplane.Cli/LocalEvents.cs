using System.Text.Json;
using Isoplane.Native;

namespace Isoplane.Cli;

internal static class LocalEvents
{
    private static readonly JsonSerializerOptions Json = new() { PropertyNamingPolicy = JsonNamingPolicy.CamelCase };

    /// <summary>
    /// Writes one event of a session: as JSON when <paramref name="json"/> is set, and otherwise as a line for a
    /// person. Logs and failures go to standard error, the rest to standard output, or all of it to
    /// <paramref name="destination"/>.
    /// </summary>
    internal static void Write(SessionEvent message, bool json, Action<string>? destination = null)
    {
        var output = destination ?? Console.WriteLine;
        var errors = destination ?? Console.Error.WriteLine;
        if (json)
        {
            (message is SessionEvent.ApplicationLog or SessionEvent.Diagnostic ? errors : output)(message.ToJson().ToJsonString());
            return;
        }
        switch (message)
        {
            case SessionEvent.ApplicationLog log:
                ApplicationLog(log.Level, log.Fields, errors);
                break;
            case SessionEvent.Diagnostic:
                break;
            case SessionEvent.Ready ready:
                output($"  ready {ready.App} at {ready.Url} (local guest execution)");
                break;
            case SessionEvent.Reloaded reloaded:
                output($"  reloaded {reloaded.App} at {reloaded.Url} (local guest execution)");
                break;
            case SessionEvent.DebuggerDetached detached:
                output($"  debugger detached; {detached.App} continues at {detached.Url}. Press Ctrl+C to stop.");
                break;
            case SessionEvent.ReloadPending:
                output("  rebuilt application; changes apply after the current request completes.");
                break;
            case SessionEvent.ReloadFailed failed:
                errors($"isoplane: reload failed; keeping the last working app: {failed.Detail}");
                break;
            case SessionEvent.BuildFailed failed:
                errors($"isoplane: rebuild failed; keeping the last working application. Edit the source to retry.\n{failed.Detail}");
                break;
            case SessionEvent.DebuggerReady debugger:
                output($"Debugger target: {debugger.Endpoint}. Connect your editor through isoplane debug-adapter; the connection remains available across HTTP requests.");
                break;
            case SessionEvent.DebuggerDetachPrepared or SessionEvent.DebuggerDetachFailed:
                throw new ArgumentException($"{message.GetType().Name} is consumed by the session that requested the detachment and has no rendering.", nameof(message));
            default:
                throw new System.Diagnostics.UnreachableException($"{message.GetType().Name} has no rendering.");
        }
    }

    private static void ApplicationLog(ulong level, string text, Action<string> output)
    {
        StructuredLog? structured = null;
        try { structured = JsonSerializer.Deserialize<StructuredLog>(text, Json); }
        catch (JsonException) { /* Guest output that does not match the log shape is displayed as plain text. */ }
        if (structured is null && level < 2) return;
        var severity = level switch { 0 => "trace", 1 => "debug", 2 => "info", 3 => "warn", 4 => structured?.Severity == "Critical" ? "crit" : "error", _ => "log" };
        var content = TerminalText(structured?.Message ?? text).Replace("\n", "\n         ", StringComparison.Ordinal);
        var category = structured?.Category;
        var exception = structured?.Exception;
        output($"  {severity,-5}  {content}"
            + (category is null ? "" : $"  [{TerminalText(category).Replace('\n', ' ')}]")
            + (exception is null ? "" : "\n         " + TerminalText(exception).Replace("\n", "\n         ", StringComparison.Ordinal)));
    }

    private sealed record StructuredLog(string? Message, string? Category, string? Exception, string? Severity);

    private static string TerminalText(string text) => string.Concat(text.Select(character =>
        char.IsControl(character) && character is not '\n' and not '\t' ? $"\\u{(int)character:x4}" : character.ToString()));
}
