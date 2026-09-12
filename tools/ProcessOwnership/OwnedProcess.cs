using System.Diagnostics;
using System.Globalization;

namespace Wisp.Processes;

internal sealed class OwnedProcess : IDisposable
{
    private readonly Process process;

    private OwnedProcess(Process process) => this.process = process;

    internal StreamReader StandardOutput => process.StandardOutput;
    internal StreamReader StandardError => process.StandardError;
    internal StreamWriter StandardInput => process.StandardInput;
    internal bool HasExited => process.HasExited;
    internal int ExitCode => process.ExitCode;
    internal int Id => process.Id;

    internal static OwnedProcess Start(string supervisor, ProcessStartInfo child, TimeSpan grace)
    {
        if (!OperatingSystem.IsLinux())
            throw new PlatformNotSupportedException("Local process ownership is currently supported on Linux.");
        var start = new ProcessStartInfo(supervisor)
        {
            WorkingDirectory = child.WorkingDirectory,
            UseShellExecute = false,
            RedirectStandardInput = true,
            RedirectStandardOutput = child.RedirectStandardOutput,
            RedirectStandardError = child.RedirectStandardError
        };
        start.Environment.Clear();
        foreach (var (key, value) in child.Environment)
            start.Environment[key] = value;
        start.ArgumentList.Add("--grace-ms");
        start.ArgumentList.Add(((long)grace.TotalMilliseconds).ToString(CultureInfo.InvariantCulture));
        start.ArgumentList.Add("--");
        start.ArgumentList.Add(child.FileName);
        foreach (var argument in child.ArgumentList)
            start.ArgumentList.Add(argument);
        return new OwnedProcess(Process.Start(start)
            ?? throw new InvalidOperationException($"Could not start process owner '{supervisor}'."));
    }

    internal Task WaitForExitAsync(CancellationToken cancellation = default) => process.WaitForExitAsync(cancellation);

    internal async Task StopAsync(TimeSpan deadline)
    {
        process.StandardInput.Close();
        await process.WaitForExitAsync().WaitAsync(deadline);
    }

    public void Dispose()
    {
        // EOF reaches the independent supervisor even if this caller dies before
        // awaiting cleanup. Killing the supervisor would discard that guarantee.
        process.StandardInput.Close();
        process.Dispose();
    }
}
