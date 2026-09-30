using System.Text.Json.Nodes;

namespace Isoplane.Cli;

internal static class LocalEvents
{
    internal static void Write(JsonObject message, bool json, Action<string>? destination = null)
    {
        var output = destination ?? Console.WriteLine;
        var errors = destination ?? Console.Error.WriteLine;
        var kind = message["event"]!.GetValue<string>();
        if (kind == "diagnostic")
        {
            if (json) errors(message.ToJsonString());
            else if (message["target"]!.GetValue<string>() == "isoplane_host::bindings::observability")
                ApplicationLog(message["fields"]!.AsObject(), errors);
            return;
        }
        if (json) output(message.ToJsonString());
        else if (kind == "reload_pending") output("  rebuilt application; changes apply after the current request completes.");
        else if (kind == "reload_failed") errors($"isoplane: reload failed; keeping the last working app: {message["detail"]}");
        else output($"  {kind} {message["app"]} at {message["url"]} (local guest execution)");
    }

    private static void ApplicationLog(JsonObject fields, Action<string> output)
    {
        if (fields["level"] is not { } levelNode || fields["fields"] is not { } data) return;
        var level = levelNode.GetValue<int>();
        var text = data.GetValue<string>();
        JsonObject? structured = null;
        try { structured = JsonNode.Parse(text) as JsonObject; }
        catch (System.Text.Json.JsonException) { /* Plain guest console output is also supported. */ }
        if (structured is null && level < 2) return;
        var severity = level switch { 0 => "trace", 1 => "debug", 2 => "info", 3 => "warn", 4 => structured?["severity"]?.GetValue<string>() == "Critical" ? "crit" : "error", _ => "log" };
        var content = TerminalText(structured?["message"]?.GetValue<string>() ?? text).Replace("\n", "\n         ", StringComparison.Ordinal);
        var category = structured?["category"]?.GetValue<string>();
        var exception = structured?["exception"]?.GetValue<string>();
        output($"  {severity,-5}  {content}"
            + (category is null ? "" : $"  [{TerminalText(category).Replace('\n', ' ')}]")
            + (exception is null ? "" : "\n         " + TerminalText(exception).Replace("\n", "\n         ", StringComparison.Ordinal)));
    }

    private static string TerminalText(string text) => string.Concat(text.Select(character =>
        char.IsControl(character) && character is not '\n' and not '\t' ? $"\\u{(int)character:x4}" : character.ToString()));
}
