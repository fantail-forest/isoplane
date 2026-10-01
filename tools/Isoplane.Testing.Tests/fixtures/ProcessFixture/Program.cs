using System.Diagnostics;
using System.Text.Json;

internal static class Program
{
    private static async Task<int> Main(string[] args)
    {
        if (args is ["wait"])
        {
            await Task.Delay(Timeout.Infinite);
            return 0;
        }
        if (args is ["exit", var code]) return int.Parse(code);
        if (args is ["cleanup-wait"])
        {
            Console.WriteLine("ready");
            await Console.In.ReadLineAsync();
            return 0;
        }
        if (args is ["cleanup-child"])
        {
            SpawnChild("cleanup-wait");
            return 0;
        }
        if (args is ["orphan"] or ["wait-for-child"])
        {
            SpawnChild();
            if (args[0] == "wait-for-child") await Task.Delay(Timeout.Infinite);
            return 0;
        }
        if (args.Length == 0 || args[0] != "dev")
            throw new ArgumentException("Provide a process fixture scenario.");

        using var settings = JsonDocument.Parse(await File.ReadAllTextAsync("process-fixture.json"));
        var scenario = settings.RootElement.GetProperty("scenario").GetString();
        switch (scenario)
        {
            case "orphan-after-eof":
            case "startup-failure":
                SpawnChild();
                break;
            case "startup-cancel":
            case "invalid-readiness":
                RecordPid(Environment.ProcessId);
                break;
            case "ready":
            case "cleanup-failure":
                break;
            default:
                throw new ArgumentException("Unknown session fixture: " + scenario);
        }
        if (scenario == "startup-failure")
        {
            Console.Error.WriteLine("application configuration failed");
            return 23;
        }
        if (scenario != "startup-cancel")
            Console.WriteLine(settings.RootElement.GetProperty("readiness").GetString());
        await Console.In.ReadToEndAsync();
        if (scenario == "cleanup-failure")
        {
            Console.Error.WriteLine(new string('0', 200) + " cleanup failed");
            return 17;
        }
        return 0;
    }

    private static void SpawnChild(string scenario = "wait")
    {
        var start = new ProcessStartInfo(Environment.ProcessPath!) { UseShellExecute = false };
        start.ArgumentList.Add(scenario);
        using var child = Process.Start(start) ?? throw new InvalidOperationException("Could not start fixture child.");
        RecordPid(child.Id);
    }

    private static void RecordPid(int pid)
    {
        File.WriteAllText("child.tmp", pid.ToString());
        File.Move("child.tmp", "child.pid");
    }
}
