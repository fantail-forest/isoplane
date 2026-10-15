using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class AsyncFramesTests
{
    [Theory]
    [InlineData("Debugger.Async.Application+<Outer>d__2", "Debugger.Async.Application.Outer")]
    [InlineData("System.IO.Stream+<<CopyToAsync>g__Core|27_0>d", "System.IO.Stream.CopyToAsync.Core")]
    [InlineData("Example.Outer`1+Inner+<Run>d__4`1[[System.Int32, System.Private.CoreLib]]", "Example.Outer.Inner.Run")]
    [InlineData("Example.Handwritten+Machine", "Example.Handwritten.Machine")]
    public void an_async_caller_without_symbols_is_named_from_its_state_machine(string stateMachine, string expected) =>
        Assert.Equal(expected, AsyncFrames.KickoffName(stateMachine));
}
