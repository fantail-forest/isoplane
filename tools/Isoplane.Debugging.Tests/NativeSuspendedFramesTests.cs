using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Newtonsoft.Json.Linq;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class NativeSuspendedFramesTests
{
    [Theory]
    [InlineData("Outer [awaiting]")]
    [InlineData("Outer [awaiting](), line 27, module guest")]
    public void awaiting_frames_expose_saved_locals_without_borrowing_live_registers_or_expressions(string name)
    {
        var state = new NativeSuspendedFrames();
        var stack = Body("""{"stackFrames":[{"id":1,"name":"Inner","instructionPointerReference":"0x123"},{"id":2,"name":"Outer [awaiting]","instructionPointerReference":"0x123"}]}""");
        stack["stackFrames"][1]!["name"] = name;
        state.Inspect("stackTrace", Body("{}"), stack);
        Assert.True(stack["stackFrames"][1]!["instructionPointerReference"] is null,
            "An awaiting caller must not claim the active method's instruction pointer.");
        var scope = Body("""{"scopes":[{"name":"Locals","variablesReference":10},{"name":"Registers","variablesReference":11}]}""");
        state.Inspect("scopes", Body("""{"frameId":2}"""), scope);
        Assert.True(scope["scopes"].Count() == 1 && scope["scopes"][0]!.Value<string>("name") == "Locals",
            "An awaiting caller must expose saved values without the active method's registers.");
        state.Inspect("variables", Body("""{"variablesReference":10}"""),
            Body("""{"variables":[{"name":"saved","variablesReference":12}]}"""));
        foreach (var command in new[] { "evaluate", "setExpression" })
        {
            var rejected = Record.Exception(() => state.Validate(command, Body("""{"frameId":2,"expression":"answer"}""")));
            Assert.True(rejected is ProtocolException, "Expressions in a suspended caller must not select its live backing frame.");
            state.Validate(command, Body("""{"frameId":1,"expression":"answer"}"""));
        }
        var assignment = Body("""{"variablesReference":12,"value":"answer"}""");
        Assert.True(Record.Exception(() => state.Validate("setVariable", assignment)) is ProtocolException,
            "Nested saved fields must not evaluate assignment expressions in another method.");
        state.Validate("setVariable", Body("""{"variablesReference":12,"value":"30"}"""));
        state.Clear();
        Assert.True(Record.Exception(() => state.Validate("setVariable", assignment)) is null,
            "Continuing must discard the previous pause's saved-variable ownership.");
    }

    [Theory]
    [InlineData("-42", true)]
    [InlineData("0x10", true)]
    [InlineData("true", true)]
    [InlineData("\"saved\"", true)]
    [InlineData("answer", false)]
    [InlineData("answer = 4", false)]
    [InlineData("Call()", false)]
    [InlineData("\"unterminated", false)]
    public void saved_assignments_distinguish_literals_from_frame_dependent_expressions(string expression, bool allowed)
    {
        var error = Record.Exception(() => SavedLocalAssignment.RequireLiteral(expression));
        Assert.True(allowed ? error is null : error is ProtocolException,
            "Saved-local assignments must accept literal values and reject expressions that need a live evaluation frame.");
    }

    [Theory]
    [InlineData("\"invalid\"")]
    [InlineData("2147483648")]
    [InlineData("-1")]
    public void malformed_frame_handles_receive_a_protocol_error(string handle)
    {
        var state = new NativeSuspendedFrames();
        var error = Record.Exception(() => state.Validate("evaluate", Body("{\"frameId\":" + handle + "}")));
        Assert.True(error is ProtocolException, "Invalid frame handles must be rejected without terminating the adapter.");
    }

    private static Dictionary<string, JToken> Body(string json) => JObject.Parse(json).Properties().ToDictionary(property => property.Name, property => property.Value);
}
