using System.Text;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class AgentSkillTests : IDisposable
{
    private readonly SkillFixture fixture = new();

    public void Dispose() => fixture.Dispose();

    [Fact]
    public void the_skill_joins_the_base_and_each_section_in_the_order_of_their_file_names()
    {
        File.WriteAllText(fixture.Base, "# Base\r\n\r\n## Documentation\r\n\r\n");
        File.WriteAllText(fixture.Section("aspnetcore"), "### AspNetCore section\n\n\n");
        File.WriteAllText(fixture.Section("opentelemetry"), "### OpenTelemetry section");
        var skill = AgentSkill.Assemble(fixture.Base, [fixture.Section("opentelemetry"), fixture.Section("aspnetcore")]);
        Assert.Equal("# Base\n\n## Documentation\n\n### AspNetCore section\n\n### OpenTelemetry section\n", Encoding.UTF8.GetString(skill));
    }

    [Fact]
    public void a_section_its_package_does_not_hold_fails_the_assembly_naming_its_file()
    {
        var missing = fixture.Section("dataprotection-efcore");
        var error = Assert.Throws<FileNotFoundException>(() => AgentSkill.Assemble(fixture.Base, [fixture.Section("aspnetcore"), missing]));
        Assert.Contains(missing, error.Message);
    }
}
