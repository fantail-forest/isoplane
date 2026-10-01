using System.Diagnostics;
using System.Xml.Linq;

/// <summary>Runs isolated MSBuild projects against the guest SDK targets.</summary>
public abstract class MsbuildTests : IDisposable
{
    protected readonly string directory = Directory.CreateTempSubdirectory("isoplane-publication-test-").FullName;
    internal static string RepositoryRoot() => Environment.GetEnvironmentVariable("ISOPLANE_BUILD_ROOT")
        ?? throw new InvalidOperationException("Run task test:managed to select the source checkout for build tests.");
    protected static string SdkDirectory() => Path.Combine(RepositoryRoot(), "guest-sdks", "dotnet");

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
        start.Environment.Remove("ISOPLANE_GUEST_RUNTIME_FEED");
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

    protected static void WriteExecutable(string path, string unix, string windows)
    {
        File.WriteAllText(path, (OperatingSystem.IsWindows() ? windows : unix) + "\n");
        if (!OperatingSystem.IsWindows()) File.SetUnixFileMode(path, UnixFileMode.UserRead | UnixFileMode.UserWrite | UnixFileMode.UserExecute);
    }

    public void Dispose() => Directory.Delete(directory, recursive: true);
}
