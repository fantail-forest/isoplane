using Xunit;

namespace Wisp.AspNetCore.Tests;

public sealed class InitializationTests
{
    [Fact]
    public void Failed_initialization_propagates_and_only_success_completes_the_hook()
    {
        Action failure = () => throw new InvalidOperationException("warmup failed");
        Initialization.Initializing += failure;
        var error = Assert.Throws<InvalidOperationException>(Initialization.Enter);
        Assert.True(error.Message == "warmup failed", "initialization must expose the original failure to preparation");
        Initialization.Initializing -= failure;
        var calls = 0;
        Initialization.Initializing += () => calls++;
        Initialization.Enter();
        Initialization.Enter();
        Assert.True(calls == 1, "successful initialization must not repeat when the entry point is invoked again");
        Assert.False(Activation.IsActivated, "template initialization must not mark a clone as activated");
    }
}
