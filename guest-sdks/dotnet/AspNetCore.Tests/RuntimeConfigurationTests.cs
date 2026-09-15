using System.Text;
using System.Text.Json;
using Xunit;

namespace Wisp.AspNetCore.Tests;

public sealed class RuntimeConfigurationTests
{
    [Theory]
    [InlineData("{\"private-key\":\"secret1\",\"private-key\":\"secret2\"}")]
    [InlineData("{\"private-key\":\"secret1\",\"PRIVATE-KEY\":\"secret2\"}")]
    public void Duplicate_keys_are_refused_without_disclosing_configuration(string json)
    {
        var error = Assert.Throws<JsonException>(() => RuntimeConfiguration.ParseValues(Encoding.UTF8.GetBytes(json)));
        Assert.True(error.Message == "Runtime configuration keys must be unique ignoring case.",
            "Invalid configuration must fail clearly without revealing a key or secret value.");
    }

    [Fact]
    public void Runtime_keys_follow_standard_case_insensitive_configuration_lookup()
    {
        var values = RuntimeConfiguration.ParseValues("{\"ConnectionStrings:Db\":\"test-value\"}"u8.ToArray());
        Assert.True(values["connectionstrings:db"] == "test-value",
            "Runtime settings must support ordinary .NET configuration key lookup.");
    }
}
