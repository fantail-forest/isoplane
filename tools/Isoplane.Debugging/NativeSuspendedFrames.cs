using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Newtonsoft.Json.Linq;

namespace Isoplane.Debugging;

internal sealed class NativeSuspendedFrames
{
    private readonly HashSet<int> frames = [];
    private readonly HashSet<int> variables = [];

    internal void Clear()
    {
        frames.Clear();
        variables.Clear();
    }

    internal void Validate(string command, IDictionary<string, JToken> arguments)
    {
        if (command is "evaluate" or "setExpression" && frames.Contains(Number(arguments, "frameId")))
            throw new ProtocolException("Expression evaluation is not supported in a suspended async caller. Use its Locals view.");
        if (command == "setVariable" && variables.Contains(Number(arguments, "variablesReference")))
        {
            if (!arguments.TryGetValue("value", out var value) || value.Type != JTokenType.String)
                throw new ProtocolException("A saved-local assignment requires a string containing a literal value.");
            SavedLocalAssignment.RequireLiteral(value.Value<string>()!);
        }
    }

    internal void Inspect(string command, IDictionary<string, JToken> arguments, IDictionary<string, JToken> body)
    {
        if (command == "stackTrace" && body.TryGetValue("stackFrames", out var stack))
        {
            foreach (var frame in stack.OfType<JObject>().Where(frame => frame.Value<string>("name")?.Contains(" [awaiting]", StringComparison.Ordinal) == true))
            {
                frames.Add(frame.Value<int>("id"));
                frame.Remove("instructionPointerReference");
            }
        }
        if (command == "scopes" && frames.Contains(Number(arguments, "frameId")) && body.TryGetValue("scopes", out var scopes))
        {
            var saved = scopes.OfType<JObject>().Where(scope => scope.Value<string>("name") == "Locals").ToArray();
            body["scopes"] = new JArray(saved);
            foreach (var scope in saved) variables.Add(scope.Value<int>("variablesReference"));
        }
        if (command == "variables" && variables.Contains(Number(arguments, "variablesReference")) && body.TryGetValue("variables", out var children))
            foreach (var child in children.OfType<JObject>())
                if (child.Value<int>("variablesReference") is > 0 and var reference) variables.Add(reference);
    }

    private static int Number(IDictionary<string, JToken> values, string name)
    {
        if (!values.TryGetValue(name, out var value) || value.Type == JTokenType.Null) return 0;
        if (value.Type != JTokenType.Integer || !int.TryParse(value.ToString(), out int number) || number < 0)
            throw new ProtocolException($"{name} must be a nonnegative integer debugger handle.");
        return number;
    }
}
