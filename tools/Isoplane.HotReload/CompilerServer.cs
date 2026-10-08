using System.Collections.Immutable;
using System.Reflection;
using System.Runtime.Loader;
using System.Text;
using Isoplane.Build;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Host.Mef;
using Microsoft.CodeAnalysis.MSBuild;
using Microsoft.CodeAnalysis.Text;

namespace Isoplane.HotReload;

internal static class CompilerServer
{
    internal static async Task<int> RunAsync(CompilerStart start)
    {
        string locatorPath = Path.Combine(start.SdkDirectory, "DotnetTools", "dotnet-format", "BuildHost-netcore", "Microsoft.Build.Locator.dll");
        var locator = AssemblyLoadContext.Default.LoadFromAssemblyPath(locatorPath).GetType("Microsoft.Build.Locator.MSBuildLocator", throwOnError: true)!;
        locator.GetMethod("RegisterMSBuildPath", [typeof(string)])!.Invoke(null, [start.SdkDirectory]);
        var assemblies = MefHostServices.DefaultAssemblies.Add(Assembly.Load("Microsoft.CodeAnalysis.Features"))
            .Add(Assembly.Load("Microsoft.CodeAnalysis.CSharp.Features"))
            .Add(Assembly.Load("Microsoft.CodeAnalysis.CSharp.Workspaces"));
        using var workspace = MSBuildWorkspace.Create(start.Properties, MefHostServices.Create(assemblies));
        using var reporting = workspace.RegisterWorkspaceFailedHandler(args => Console.Error.WriteLine(args.Diagnostic));
        var project = await workspace.OpenProjectAsync(start.Project);
        var solution = project.Solution;
        foreach (var input in solution.Projects)
        {
            if (!start.AssemblyPaths.TryGetValue(input.AssemblyName ?? "", out string? assembly))
                throw new IOException($"The paused application has no baseline assembly for project '{input.Name}'.");
            var output = input.CompilationOutputInfo;
            string generated = output.GeneratedFilesOutputDirectory ?? Path.GetDirectoryName(output.AssemblyPath)
                ?? throw new IOException($"Project '{input.Name}' did not identify its generated-source directory.");
            // File-local generated type identities depend on their original path, even when the baseline assembly is snapshotted.
            solution = solution.WithProjectCompilationOutputInfo(input.Id,
                output.WithGeneratedFilesOutputDirectory(generated).WithAssemblyPath(assembly));
        }
        await Task.WhenAll(solution.Projects.SelectMany(project => project.Documents).Select(document => document.GetTextAsync()));
        using var compiler = new EditCompiler(workspace, solution, project.Id, start.Capabilities);
        await HotReloadProtocol.WriteAsync(Console.Out, new CompilerReply([], [], false)
        {
            Documents = solution.Projects.SelectMany(project => project.Documents).Select(document => document.FilePath)
                .OfType<string>().Distinct().ToArray()
        });
        while (await HotReloadProtocol.ReadAsync<CompilerCommand>(Console.In) is { } command)
        {
            CompilerReply reply;
            switch (command.Action)
            {
                case "update" when command.Edit is { } edit:
                    var updated = await ReplaceSourcesAsync(solution, edit.Sources);
                    reply = await compiler.EmitAsync(updated, edit);
                    if (!reply.Pending && !reply.RestartRequired && reply.Diagnostics.Length == 0) solution = updated;
                    break;
                case "commit":
                    solution = compiler.Commit();
                    reply = new([], [], false);
                    break;
                case "discard":
                    compiler.Discard();
                    reply = new([], [], false);
                    break;
                case "stop":
                    return 0;
                default:
                    throw new IOException("The debugger sent an unsupported Hot Reload compiler command.");
            }
            await HotReloadProtocol.WriteAsync(Console.Out, reply);
        }
        return 0;
    }

    private static async Task<Solution> ReplaceSourcesAsync(Solution solution, SourceUpdate[] updates)
    {
        foreach (var update in updates)
        {
            var documents = solution.GetDocumentIdsWithFilePath(update.Path);
            if (documents.IsEmpty) throw new IOException($"Source '{update.Path}' is not part of the paused project's compilation.");
            foreach (var id in documents)
            {
                var original = await solution.GetDocument(id)!.GetTextAsync();
                solution = solution.WithDocumentText(id, SourceText.From(update.Text, original.Encoding ?? Encoding.UTF8, original.ChecksumAlgorithm));
            }
        }
        return solution;
    }
}
