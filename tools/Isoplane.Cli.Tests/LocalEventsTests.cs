using System.Text.Json.Nodes;
using Isoplane.Cli;
using Isoplane.Native;
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
    [InlineData(0UL)]
    [InlineData(1UL)]
    public void plain_and_nonconforming_logs_below_information_are_suppressed(ulong level)
    {
        Assert.Empty(Display(level, "console output"));
        Assert.Empty(Display(level, "{\"message\":42}"));
    }

    [Theory]
    [InlineData(0UL, "Trace", "trace")]
    [InlineData(1UL, "Debug", "debug")]
    [InlineData(2UL, "Information", "info ")]
    [InlineData(3UL, "Warning", "warn ")]
    [InlineData(4UL, "Error", "error")]
    [InlineData(4UL, "Critical", "crit ")]
    [InlineData(9UL, "Critical", "log  ")]
    public void structured_logs_keep_the_host_level_and_framework_critical_severity(ulong level, string severity, string expected)
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
    public void json_output_names_each_event_beside_its_members()
    {
        var output = new List<string>();
        SessionEvent[] events =
        [
            new SessionEvent.Ready("shop", "http://127.0.0.1:5000"), new SessionEvent.Reloaded("shop", "http://127.0.0.1:5000"),
            new SessionEvent.DebuggerDetached("shop", "http://127.0.0.1:5000"), new SessionEvent.ReloadFailed("the module does not validate"),
            new SessionEvent.ApplicationLog(0, "{\"message\":{},\"severity\":false}"), new SessionEvent.Diagnostic("INFO isoplane_host::cell: activated instance=7"),
            new SessionEvent.BuildFailed("error CS1002: ; expected"), new SessionEvent.ReloadPending(),
            new SessionEvent.DebuggerReady("127.0.0.1:4711", "/sdk/runtime.wasm", "/app/application.tar", "mono"),
        ];

        foreach (var message in events) LocalEvents.Write(message, json: true, output.Add);

        string[] expected =
        [
            """{"event":"ready","app":"shop","url":"http://127.0.0.1:5000"}""",
            """{"event":"reloaded","app":"shop","url":"http://127.0.0.1:5000"}""",
            """{"event":"debugger_detached","app":"shop","url":"http://127.0.0.1:5000"}""",
            """{"event":"reload_failed","detail":"the module does not validate"}""",
            """{"event":"application_log","level":0,"fields":"{\"message\":{},\"severity\":false}"}""",
            """{"event":"diagnostic","text":"INFO isoplane_host::cell: activated instance=7"}""",
            """{"event":"build_failed","detail":"error CS1002: ; expected"}""",
            """{"event":"reload_pending"}""",
            """{"event":"debugger_ready","endpoint":"127.0.0.1:4711","module":"/sdk/runtime.wasm","package":"/app/application.tar","runtime":"mono"}""",
        ];
        Assert.Equal(expected.Length, output.Count);
        foreach (var (wanted, written) in expected.Zip(output)) AssertSameJson(wanted, written);
    }

    [Fact]
    public void an_event_json_output_wrote_reads_back_as_the_same_event()
    {
        var written = new SessionEvent.ApplicationLog(3, "disk almost full");

        Assert.Equal(written, SessionEvent.Parse(written.ToJson()));
    }

    private static void AssertSameJson(string expected, string actual) =>
        Assert.True(JsonNode.DeepEquals(JsonNode.Parse(expected), JsonNode.Parse(actual)), $"Expected {expected}, and the output was {actual}.");

    private static List<string> Display(ulong level, string text)
    {
        var output = new List<string>();
        LocalEvents.Write(new SessionEvent.ApplicationLog(level, text), json: false, output.Add);
        return output;
    }
}
