using System.Text;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Newtonsoft.Json.Linq;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class NativeDebuggerRequestsTests
{
    [Fact]
    public void guest_launch_preserves_the_application_environment_without_debugger_python_settings()
    {
        var original = new Dictionary<string, string> { ["PATH"] = "/customer/bin", ["KEEP"] = "original", ["REMOVE"] = "original" };
        var arguments = new Dictionary<string, JToken>
        {
            ["env"] = new JObject { ["KEEP"] = "override", ["REMOVE"] = JValue.CreateNull(), ["NEW"] = "value" }
        };
        NativeDebuggerRequests.ConfigureLaunch(arguments, original);
        var environment = (JObject)arguments["env"];
        Assert.True(((JArray)arguments["initCommands"]).Values<string>().Contains("settings set target.inherit-env false") && environment.Value<string>("PATH") == "/customer/bin"
            && !environment.ContainsKey("PYTHONHOME"), "The application must inherit the CLI environment, not LLDB's bundled Python or library paths.");
        Assert.True(environment.Value<string>("KEEP") == "override" && environment.Value<string>("NEW") == "value"
            && !environment.ContainsKey("REMOVE"), "Explicit launch environment overrides and removals must still apply.");
        Assert.True(original["KEEP"] == "original" && original.ContainsKey("REMOVE"), "Launch overrides must not mutate the CLI environment snapshot.");
    }

    [Fact]
    public void managed_expressions_cross_the_command_boundary_as_data()
    {
        const string expression = "\"quoted\"; `script print('unwanted')\n🌲";
        var encoded = NativeDebuggerRequests.Encode("evaluate", new Dictionary<string, JToken> { ["expression"] = expression });
        var command = encoded["expression"].Value<string>()!;
        var payload = JObject.Parse(Encoding.UTF8.GetString(Convert.FromBase64String(command.Split(' ')[1])));
        Assert.True(payload["arguments"]!.Value<string>("expression") == expression,
            "Editor expressions must reach the managed helper unchanged without becoming LLDB commands.");
    }

    [Theory]
    [InlineData("(lldb) isoplane-request payload\n{\"body\":{\"variables\":[]}}\n")]
    [InlineData("{\"body\":{\"variables\":[]}}\r\n")]
    public void managed_responses_allow_the_debuggers_command_echo(string result)
    {
        var body = NativeDebuggerRequests.Decode(new Dictionary<string, JToken> { ["result"] = result });
        Assert.True(body["variables"] is JArray { Count: 0 }, "LLDB command echo must not corrupt the managed response.");
    }

    [Theory]
    [InlineData("")]
    [InlineData("Traceback: helper unavailable")]
    [InlineData("{}")]
    [InlineData("{\"error\":\"The selected stack has no guest memory context.\"}")]
    public void failed_managed_requests_return_protocol_errors(string result)
    {
        var error = Record.Exception(() => NativeDebuggerRequests.Decode(new Dictionary<string, JToken> { ["result"] = result }));
        Assert.True(error is ProtocolException, "Missing or failed managed helper responses must fail the request without terminating the adapter.");
    }
    [Fact]
    public void native_capabilities_do_not_advertise_unimplemented_managed_operations()
    {
        var body = new Dictionary<string, JToken>
        {
            ["supportsDataBreakpoints"] = true,
            ["supportsStepInTargetsRequest"] = true,
            ["exceptionBreakpointFilters"] = new JArray(new JObject { ["filter"] = "cpp_throw" })
        };
        NativeDebuggerRequests.Capabilities(body);
        Assert.True(!body["supportsDataBreakpoints"].Value<bool>() && !body["supportsStepInTargetsRequest"].Value<bool>()
            && body["exceptionBreakpointFilters"] is JArray { Count: 0 },
            "Native C++ capabilities must not promise managed operations that the adapter cannot deliver.");
    }

}
