using System.Text.Json.Nodes;

namespace Wisp.Cli;

internal static class LocalEvents
{
    internal static void Write(JsonObject message, bool json)
    {
        var kind = message["event"]!.GetValue<string>();
        if (kind == "diagnostic")
        {
            if (json) Console.Error.WriteLine(message.ToJsonString());
            else if (message["target"]!.GetValue<string>() == "wisp_host::bindings::observability")
                ApplicationLog(message["fields"]!.AsObject());
            return;
        }
        if (json) Console.WriteLine(message.ToJsonString());
        else if (kind == "reload_failed") Console.Error.WriteLine($"isoplane: reload failed; keeping the last working app: {message["detail"]}");
        else Console.WriteLine($"  {kind} {message["app"]} at {message["url"]} (local guest execution)");
    }

    private static void ApplicationLog(JsonObject fields)
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
        Console.Error.WriteLine($"  {severity,-5}  {content}"
            + (category is null ? "" : $"  [{TerminalText(category).Replace('\n', ' ')}]")
            + (exception is null ? "" : "\n         " + TerminalText(exception).Replace("\n", "\n         ", StringComparison.Ordinal)));
    }

    private static string TerminalText(string text) => string.Concat(text.Select(character =>
        char.IsControl(character) && character is not '\n' and not '\t' ? $"\\u{(int)character:x4}" : character.ToString()));
}
