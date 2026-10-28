using Microsoft.CodeAnalysis.Diagnostics;
using Xunit;

namespace Isoplane.Analyzers.Tests;

public sealed class HelpLinkTests
{
    [Fact]
    public void every_diagnostic_links_to_its_section_of_the_error_code_reference()
    {
        var rules = typeof(BlockingCallAnalyzer).Assembly.GetExportedTypes()
            .Where(type => !type.IsAbstract && typeof(DiagnosticAnalyzer).IsAssignableFrom(type))
            .SelectMany(type => ((DiagnosticAnalyzer)Activator.CreateInstance(type)!).SupportedDiagnostics).ToArray();
        Assert.NotEmpty(rules);
        Assert.All(rules, rule => Assert.Equal($"https://isoplane.dev/docs/reference/error-codes#{rule.Id.ToLowerInvariant()}", rule.HelpLinkUri));
    }
}
