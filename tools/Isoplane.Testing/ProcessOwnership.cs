using System.Diagnostics;
using System.Globalization;
using System.Text.Json;

namespace Wisp.Testing;

internal static class ProcessOwnership
{
    // argv carries every user-supplied path. The shell waits for verification before executing it.
    internal const string Launch = "printf '{\"event\":\"process-owner\",\"pid\":%s}\\n' \"$$\"; IFS= read -r token && [ \"$token\" = start ] || exit 1; exec \"$@\"";

    internal static async Task ValidateCleanupToolAsync(string executable, TimeSpan timeout, CancellationToken cancellation)
    {
        var start = new ProcessStartInfo(executable) { UseShellExecute = false, RedirectStandardError = true };
        foreach (var argument in new[] { "-0", Environment.ProcessId.ToString(CultureInfo.InvariantCulture) })
            start.ArgumentList.Add(argument);
        try
        {
            using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start the Linux cleanup tool.");
            using var deadline = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            deadline.CancelAfter(timeout);
            var error = process.StandardError.ReadToEndAsync(deadline.Token);
            try
            {
                await process.WaitForExitAsync(deadline.Token);
                var detail = await error;
                if (process.ExitCode != 0)
                    throw new InvalidOperationException($"Linux cleanup tool '{executable}' could not inspect this process: {detail}");
            }
            finally
            {
                if (!process.HasExited)
                {
                    process.Kill();
                    await process.WaitForExitAsync();
                }
            }
        }
        catch (System.ComponentModel.Win32Exception error)
        {
            throw new InvalidOperationException($"Linux cleanup tool '{executable}' is unavailable; install an executable kill before starting an application session.", error);
        }
    }

    internal static async Task<int> VerifyAsync(StreamReader output, CancellationToken cancellation)
    {
        var line = await output.ReadLineAsync(cancellation)
            ?? throw new InvalidOperationException("setsid did not establish an owned process session.");
        using var record = JsonDocument.Parse(line);
        if (record.RootElement.GetProperty("event").GetString() != "process-owner")
            throw new InvalidOperationException("The process session did not supply its ownership handshake.");
        var pid = record.RootElement.GetProperty("pid").GetInt32();
        if (pid <= 1 || pid == Environment.ProcessId)
            throw new InvalidOperationException("The process session supplied an invalid owner.");
        var stat = await File.ReadAllTextAsync($"/proc/{pid}/stat", cancellation);
        var fields = stat[(stat.LastIndexOf(')') + 2)..].Split(' ', StringSplitOptions.RemoveEmptyEntries);
        if (fields.Length < 4 || fields[2] != pid.ToString(CultureInfo.InvariantCulture) || fields[3] != pid.ToString(CultureInfo.InvariantCulture))
            throw new InvalidOperationException("The CLI process is not the leader of its own process group and session.");
        return pid;
    }

    internal static async Task<bool> TerminateAsync(int group, TimeSpan timeout)
    {
        using var deadline = new CancellationTokenSource(timeout);
        if (!(await SignalAsync(group, "0", deadline.Token)).Success)
            return false;
        var killed = await SignalAsync(group, "KILL", deadline.Token);
        if (!killed.Success && (await SignalAsync(group, "0", deadline.Token)).Success)
            throw new InvalidOperationException($"Could not terminate owned CLI process group {group}: {killed.Error}");
        return killed.Success;
    }

    private static async Task<(bool Success, string Error)> SignalAsync(int group, string signal, CancellationToken cancellation)
    {
        var start = new ProcessStartInfo("kill") { UseShellExecute = false, RedirectStandardError = true };
        foreach (var argument in new[] { $"-{signal}", "--", $"-{group.ToString(CultureInfo.InvariantCulture)}" })
            start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start Linux kill for owned process cleanup.");
        var error = process.StandardError.ReadToEndAsync();
        try
        {
            await process.WaitForExitAsync(cancellation);
            var detail = await error;
            return (process.ExitCode == 0, detail);
        }
        finally
        {
            if (!process.HasExited)
            {
                process.Kill();
                await process.WaitForExitAsync();
            }
        }
    }
}
