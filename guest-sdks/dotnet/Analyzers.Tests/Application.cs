using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CodeActions;
using Microsoft.CodeAnalysis.CodeFixes;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Diagnostics;
using Microsoft.CodeAnalysis.Formatting;
using Microsoft.CodeAnalysis.Text;
using Xunit;

namespace Isoplane.Analyzers.Tests;

/// <summary>Application source as an editor holds it: compiled against the SDK, analyzed, and fixed.</summary>
internal static class Application
{
    private static readonly ImmutableArray<MetadataReference> References =
    [
        .. ((string)AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES")!).Split(Path.PathSeparator)
            .Where(path => !Path.GetFileName(path).StartsWith("Isoplane.", StringComparison.Ordinal))
            .Select(path => (MetadataReference)MetadataReference.CreateFromFile(path)),
        MetadataReference.CreateFromFile(typeof(Request).Assembly.Location)
    ];

    private static readonly ImmutableArray<DiagnosticAnalyzer> Analyzers = [new EntryMethodAnalyzer(), new HostRunAnalyzer(), new BlockingCallAnalyzer()];

    private static readonly ImmutableArray<CodeFixProvider> Fixes = [new EntryMethodFixes(), new AwaitFixes()];

    /// <summary>The members of a handler application's one class, in a file with the usings its templates have.</summary>
    internal static string Library(string members) => $$"""
        using System;
        using System.Threading.Tasks;
        using Isoplane;

        public static class App
        {
        {{string.Join(Environment.NewLine, members.Split('\n').Select(line => line.Length == 0 ? line : "    " + line.TrimEnd('\r')))}}
        }
        """.ReplaceLineEndings("\n") + "\n";

    internal static Project Project(string source, OutputKind kind = OutputKind.DynamicallyLinkedLibrary)
    {
        var workspace = new AdhocWorkspace();
        var options = workspace.CurrentSolution.Options.WithChangedOption(FormattingOptions.NewLine, LanguageNames.CSharp, "\n");
        Assert.True(workspace.TryApplyChanges(workspace.CurrentSolution.WithOptions(options)));
        var project = workspace.AddProject(ProjectInfo.Create(ProjectId.CreateNewId(), VersionStamp.Default, "App", "App", LanguageNames.CSharp,
            compilationOptions: new CSharpCompilationOptions(kind, nullableContextOptions: NullableContextOptions.Enable),
            parseOptions: new CSharpParseOptions(LanguageVersion.Latest), metadataReferences: References));
        return workspace.AddDocument(project.Id, "App.cs", SourceText.From(source)).Project;
    }

    /// <summary>The compiler's errors and the analyzer's diagnostics for the project, in source order.</summary>
    internal static async Task<ImmutableArray<Diagnostic>> DiagnosticsAsync(Project project)
    {
        var compilation = (await project.GetCompilationAsync())!;
        var all = await compilation.WithAnalyzers(Analyzers).GetAllDiagnosticsAsync();
        return [.. all.Where(diagnostic => diagnostic.Severity == DiagnosticSeverity.Error || diagnostic.Id.StartsWith("ISOPLANE", StringComparison.Ordinal))
            .OrderBy(diagnostic => diagnostic.Location.SourceSpan.Start)];
    }

    /// <summary>The identifiers the analyzer reports for <paramref name="source"/>, which must otherwise compile.</summary>
    internal static async Task<string[]> AnalyzeAsync(string source, OutputKind kind = OutputKind.DynamicallyLinkedLibrary)
    {
        var diagnostics = await DiagnosticsAsync(Project(source, kind));
        Assert.True(diagnostics.All(diagnostic => diagnostic.Id.StartsWith("ISOPLANE", StringComparison.Ordinal)),
            "the case must compile: " + string.Join("; ", diagnostics));
        return [.. diagnostics.Select(diagnostic => diagnostic.Id)];
    }

    /// <summary>
    /// Applies the fix an editor offers for the first diagnostic named <paramref name="id"/> and returns the
    /// file as it is afterwards. Null when no fix is offered.
    /// </summary>
    internal static async Task<string?> FixAsync(string source, string id, OutputKind kind = OutputKind.DynamicallyLinkedLibrary)
    {
        var changed = await FixAsync(Project(source, kind).Documents.Single(), id);
        return changed is null ? null : (await changed.GetTextAsync()).ToString();
    }

    /// <summary>Applies the offered fix in a document, retaining the project's other source files.</summary>
    internal static async Task<Document?> FixAsync(Document document, string id)
    {
        var tree = await document.GetSyntaxTreeAsync();
        var diagnostic = (await DiagnosticsAsync(document.Project)).First(candidate => candidate.Id == id && candidate.Location.SourceTree == tree);
        var actions = new List<CodeAction>();
        foreach (var provider in Fixes.Where(provider => provider.FixableDiagnosticIds.Contains(id)))
            await provider.RegisterCodeFixesAsync(new CodeFixContext(document, diagnostic, (action, _) => actions.Add(action), CancellationToken.None));
        if (actions.Count == 0) return null;
        var applied = Assert.Single((await Assert.Single(actions).GetOperationsAsync(CancellationToken.None)).OfType<ApplyChangesOperation>());
        return applied.ChangedSolution.GetDocument(document.Id);
    }
}
