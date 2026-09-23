using System.Text;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Newtonsoft.Json;
using Newtonsoft.Json.Linq;

namespace Isoplane.Debugging;

internal static class NativeDebuggerRequests
{
    internal static bool IsManaged(string command) => command is "stackTrace" or "scopes" or "variables" or "setVariable" or "evaluate" or "dataBreakpointInfo";

    internal static IDictionary<string, JToken> Encode(string command, IDictionary<string, JToken> arguments)
    {
        var request = JsonConvert.SerializeObject(new { command, arguments });
        return new Dictionary<string, JToken>
        {
            ["context"] = "repl",
            ["expression"] = "`isoplane-request " + Convert.ToBase64String(Encoding.UTF8.GetBytes(request))
        };
    }

    internal static IDictionary<string, JToken> Decode(IDictionary<string, JToken> response)
    {
        var text = response.TryGetValue("result", out var result) ? result.Value<string>() : null;
        var line = text?.Split('\n', StringSplitOptions.RemoveEmptyEntries).LastOrDefault();
        if (line is null) throw new ProtocolException("LLDB returned no managed debugger response.");
        JObject envelope;
        try { envelope = JObject.Parse(line); }
        catch (JsonException error) { throw new ProtocolException($"LLDB could not run its managed debugger helper: {error.Message}"); }
        if (envelope.Value<string>("error") is { } message) throw new ProtocolException(message);
        if (envelope["body"] is not JObject body) throw new ProtocolException("LLDB returned an invalid managed debugger response.");
        return body.Properties().ToDictionary(property => property.Name, property => property.Value);
    }

    internal static void ConfigureLaunch(IDictionary<string, JToken> arguments, IReadOnlyDictionary<string, string> environment)
    {
        var inherited = new Dictionary<string, string>(environment,
            OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal);
        if (arguments.TryGetValue("env", out var configured) && configured is JObject overrides)
            foreach (var variable in overrides.Properties())
            {
                if (variable.Value.Type == JTokenType.Null) inherited.Remove(variable.Name);
                else inherited[variable.Name] = variable.Value.Value<string>()
                    ?? throw new ProtocolException($"Invalid environment value for {variable.Name}.");
            }
        arguments["env"] = JObject.FromObject(inherited);
        arguments["disableASLR"] = false;
        arguments["initCommands"] = new JArray(
            "settings set target.inherit-env false",
            "settings set symbols.enable-external-lookup false",
            "settings set plugin.jit-loader.gdb.enable on",
            "settings set target.inline-breakpoint-strategy always",
            "script import os, sys; sys.path.insert(0, os.environ['ISOPLANE_NATIVE_DEBUG_DIRECTORY']); import isoplane_lldb; isoplane_lldb.__lldb_init_module(lldb.debugger, {})");
        arguments["preRunCommands"] = new JArray(
            "process handle -s false -n false -p true SIGSEGV SIGILL");
    }

    internal static void Capabilities(IDictionary<string, JToken> body)
    {
        body["supportsCancelRequest"] = false;
        body["supportsCompletionsRequest"] = false;
        body["supportsExceptionInfoRequest"] = false;
        body["supportsExceptionOptions"] = false;
        body["supportsDataBreakpoints"] = false;
        body["supportsStepInTargetsRequest"] = false;
        body["supportsSetExpression"] = false;
        body["exceptionBreakpointFilters"] = new JArray();
    }
}
