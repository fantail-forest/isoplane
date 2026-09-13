using System.Diagnostics;
using System.Runtime.CompilerServices;
using System.Xml.Linq;

/// <summary>Runs isolated MSBuild projects against the guest SDK targets.</summary>
public abstract class MsbuildTests : IDisposable
{
    protected readonly string directory = Directory.CreateTempSubdirectory("wisp-publication-test-").FullName;
    protected static string SdkDirectory() => Path.GetFullPath(Path.Combine(Path.GetDirectoryName(SourcePath())!, "..", ".."));
    private static string SourcePath([CallerFilePath] string source = "") => source;

    protected string WriteProject(params XElement[] elements)
    {
        var project = Path.Combine(directory, "test.proj");
        new XDocument(new XElement("Project", elements)).Save(project);
        return project;
    }

    protected static ProcessStartInfo Msbuild(string project, string argument)
    {
        var start = new ProcessStartInfo("dotnet") { WorkingDirectory = Path.GetDirectoryName(project)!, UseShellExecute = false, RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var arg in new[] { "msbuild", project, "-nologo", "-nodeReuse:false", argument }) start.ArgumentList.Add(arg);
        start.Environment.Remove("WISP_GUEST_RUNTIME_FEED");
        return start;
    }

    protected static async Task<(int Exit, string Output)> Run(ProcessStartInfo start)
    {
        using var process = Process.Start(start)!;
        var output = process.StandardOutput.ReadToEndAsync();
        var error = process.StandardError.ReadToEndAsync();
        using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(90));
        try
        {
            await process.WaitForExitAsync(timeout.Token);
            return (process.ExitCode, await output + await error);
        }
        finally
        {
            if (!process.HasExited) process.Kill(entireProcessTree: true);
            await process.WaitForExitAsync();
        }
    }

    protected static void WriteExecutable(string path, string text)
    {
        File.WriteAllText(path, text + "\n");
        if (!OperatingSystem.IsWindows()) File.SetUnixFileMode(path, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
    }

    public void Dispose() => Directory.Delete(directory, recursive: true);
}
