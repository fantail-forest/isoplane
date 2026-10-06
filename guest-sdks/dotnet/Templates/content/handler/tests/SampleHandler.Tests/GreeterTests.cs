namespace SampleHandler.Tests;

/// <summary>Ordinary unit tests: application logic runs on the test machine, outside the sandbox.</summary>
public sealed class GreeterTests
{
    private readonly Greeter greeter = new("world");

    [Fact]
    public void A_named_caller_is_greeted_by_name()
    {
        Assert.Equal(new Greeting("Hello, Ada!", "Ada"), greeter.Greet(" Ada "));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    public void A_caller_without_a_name_gets_the_default(string? name)
    {
        Assert.Equal(new Greeting("Hello, world!", "world"), greeter.Greet(name));
    }
}
