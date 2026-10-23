using System.Text.Json;

namespace Isoplane.Cli;

internal sealed record SourceInputs(string[] Files, string[] OutputDirectories)
{
    internal static StringComparer PathComparer => OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal;

    internal static async Task<SourceInputs> ReadAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        string project;
        try { project = ManagedBundle.ProjectFile(options.Directory); }
        catch (ArgumentException error)
        {
            throw new InvalidOperationException($"Could not discover source files in '{options.Directory}': {error.Message}", error);
        }
        using var process = Processes.Start("dotnet", ["watch", "--list", "--project", project,
            "--configuration", options.Configuration, "--property:IsoplaneManagedBundleBuild=true"], options.Directory, captureOutput: true, captureErrors: true);
        var output = process.StandardOutput.ReadToEndAsync(cancellation);
        var errors = process.StandardError.ReadToEndAsync(cancellation);
        try
        {
            await process.WaitForExitAsync(cancellation);
            var text = await output;
            var diagnostics = await errors;
            if (process.ExitCode != 0)
                throw new InvalidOperationException($"Could not discover source files for '{project}' (exit {process.ExitCode}).{Environment.NewLine}{text}{diagnostics}");
            var paths = text.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries)
                .Where(Path.IsPathFullyQualified).Select(Path.GetFullPath).Distinct(PathComparer).ToArray();
            if (!paths.Contains(project, PathComparer))
                throw new InvalidOperationException($"The .NET SDK returned no watch inputs for '{project}'.{Environment.NewLine}{text}{diagnostics}");
            if (!string.IsNullOrWhiteSpace(diagnostics)) Console.Error.Write(diagnostics);
            var inputs = paths.ToHashSet(PathComparer);
            var outputs = new HashSet<string>(PathComparer) { new LocalProject(options.Directory).Root };
            foreach (var reference in paths.Where(path => Path.GetExtension(path).Equals(".csproj", StringComparison.OrdinalIgnoreCase)))
            {
                var build = await BuildInputsAsync(reference, options, cancellation);
                inputs.UnionWith(build.Files);
                outputs.UnionWith(build.OutputDirectories);
            }
            return new(inputs.ToArray(), outputs.ToArray());
        }
        finally
        {
            await Processes.StopAsync(process);
            await Task.WhenAll(output, errors);
        }
    }

    private static async Task<SourceInputs> BuildInputsAsync(string project, DevelopmentOptions options, CancellationToken cancellation)
    {
        using var process = Processes.Start("dotnet", ["msbuild", project, "-nologo", "-property:Configuration=" + options.Configuration,
            "-property:IsoplaneManagedBundleBuild=true", "-getProperty:DirectoryBuildPropsPath,DirectoryBuildTargetsPath,BaseOutputPath,BaseIntermediateOutputPath,ArtifactsPath"],
            options.Directory, captureOutput: true, captureErrors: true);
        var output = process.StandardOutput.ReadToEndAsync(cancellation);
        var errors = process.StandardError.ReadToEndAsync(cancellation);
        try
        {
            await process.WaitForExitAsync(cancellation);
            var text = await output;
            var diagnostics = await errors;
            if (process.ExitCode != 0)
                throw new InvalidOperationException($"Could not discover build inputs for '{project}' (exit {process.ExitCode}).{Environment.NewLine}{text}{diagnostics}");
            if (!string.IsNullOrWhiteSpace(diagnostics)) Console.Error.Write(diagnostics);
            using var evaluation = JsonDocument.Parse(text);
            var properties = evaluation.RootElement.GetProperty("Properties");
            string[] Paths(params string[] names) => names.Select(name => properties.GetProperty(name).GetString())
                .OfType<string>().Where(path => path.Length != 0)
                .Select(path => Path.GetFullPath(path.Replace('\\', Path.DirectorySeparatorChar), Path.GetDirectoryName(project)!)).ToArray();
            return new(Paths("DirectoryBuildPropsPath", "DirectoryBuildTargetsPath"),
                Paths("BaseOutputPath", "BaseIntermediateOutputPath", "ArtifactsPath"));
        }
        catch (Exception error) when (error is JsonException or KeyNotFoundException or InvalidOperationException)
        {
            throw new InvalidOperationException($"Could not read evaluated build inputs for '{project}': {error.Message}", error);
        }
        finally
        {
            await Processes.StopAsync(process);
            await Task.WhenAll(output, errors);
        }
    }
}
