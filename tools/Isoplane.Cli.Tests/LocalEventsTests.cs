using System.Text.Json.Nodes;
using Isoplane.Cli;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class LocalEventsTests
{
    public static TheoryData<string, string> NonStringFields
    {
        get
        {
            var cases = new TheoryData<string, string>();
            foreach (var property in new[] { "message", "category", "exception", "severity" })
                foreach (var value in new[] { "{}", "[]", "42", "true", "false" })
                    cases.Add(property, value);
            return cases;
        }
    }

    [Theory]
    [MemberData(nameof(NonStringFields))]
    public void unexpected_field_kinds_display_the_original_guest_text(string property, string value)
    {
        var payload = new JsonObject
        {
            ["message"] = "hello",
            ["category"] = "Orders",
            ["exception"] = "failed",
            ["severity"] = "Critical"
        };
        payload[property] = JsonNode.Parse(value);
        var text = payload.ToJsonString();

        Assert.Equal(["  error  " + text], Display(4, text));
    }

    [Theory]
    [InlineData("message", "{\"message\":null}")]
    [InlineData("category", "hello")]
    [InlineData("exception", "hello")]
    [InlineData("severity", "hello")]
    public void null_fields_keep_their_optional_string_behavior(string property, string expected)
    {
        var payload = new JsonObject { ["message"] = "hello" };
        payload[property] = null;

        Assert.Equal(["  error  " + expected], Display(4, payload.ToJsonString()));
    }

    [Theory]
    [InlineData("console output")]
    [InlineData("{not json}")]
    [InlineData("[]")]
    [InlineData("42")]
    [InlineData("true")]
    [InlineData("false")]
    [InlineData("null")]
    [InlineData("\"a JSON string\"")]
    public void plain_text_and_other_json_roots_display_without_interpretation(string text)
    {
        Assert.Equal(["  info   " + text], Display(2, text));
    }

    [Theory]
    [InlineData(0)]
    [InlineData(1)]
    public void plain_and_nonconforming_logs_below_information_are_suppressed(int level)
    {
        Assert.Empty(Display(level, "console output"));
        Assert.Empty(Display(level, "{\"message\":42}"));
    }

    [Theory]
    [InlineData(0, "Trace", "trace")]
    [InlineData(1, "Debug", "debug")]
    [InlineData(2, "Information", "info ")]
    [InlineData(3, "Warning", "warn ")]
    [InlineData(4, "Error", "error")]
    [InlineData(4, "Critical", "crit ")]
    [InlineData(9, "Critical", "log  ")]
    public void structured_logs_keep_the_host_level_and_framework_critical_severity(int level, string severity, string expected)
    {
        var payload = new JsonObject { ["message"] = "hello", ["severity"] = severity, ["properties"] = new JsonObject { ["count"] = 1 } };

        Assert.Equal([$"  {expected}  hello"], Display(level, payload.ToJsonString()));
    }

    [Fact]
    public void structured_and_plain_logs_escape_terminal_controls_and_indent_new_lines()
    {
        var payload = new JsonObject { ["message"] = "hello\u001b[2J\nworld", ["category"] = "Orders\r\nnext", ["exception"] = "failed\u0000\nagain" };

        Assert.Equal(["  info   hello\\u001b[2J\n         world  [Orders\\u000d next]\n         failed\\u0000\n         again"],
            Display(2, payload.ToJsonString()));
        Assert.Equal(["  info   console\\u001b[2J\n         next\tline\\u000d"], Display(2, "console\u001b[2J\nnext\tline\r"));
    }

    [Fact]
    public void json_output_preserves_the_complete_guest_diagnostic()
    {
        var diagnostic = Diagnostic(0, "{\"message\":{},\"severity\":false}");
        var output = new List<string>();

        LocalEvents.Write(diagnostic, json: true, output.Add);

        Assert.Equal([diagnostic.ToJsonString()], output);
    }

    private static List<string> Display(int level, string text)
    {
        var output = new List<string>();
        LocalEvents.Write(Diagnostic(level, text), json: false, output.Add);
        return output;
    }

    private static JsonObject Diagnostic(int level, string text) => new()
    {
        ["event"] = "diagnostic",
        ["target"] = "isoplane_host::bindings::observability",
        ["fields"] = new JsonObject { ["level"] = level, ["fields"] = text }
    };
}
