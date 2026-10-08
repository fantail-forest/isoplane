using System.Net;
using System.Net.Sockets;
using Mono.Debugger.Soft;
using Xunit;

namespace Isoplane.Debugging.Tests;

internal static class HotReloadAcceptance
{
    internal static async Task<int> RunAsync(string endpoint, string url, string deltas, string source)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(3));
        using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp)
            { LingerState = new LingerOption(true, 0) };
        using var cancelTransport = deadline.Token.Register(socket.Dispose);
        using var http = new HttpClient { Timeout = Timeout.InfiniteTimeSpan };
        await using var owner = new GuestSession();
        var (transport, _) = await owner.ConnectAsync(IPEndPoint.Parse(endpoint)).WaitAsync(deadline.Token);
        Console.WriteLine("The debug session allocated its guest transport.");
        var connecting = VirtualMachineManager.ConnectInternalAsync(socket, null, transport, null);
        var response = http.GetStringAsync(url, deadline.Token);
        Console.WriteLine("The Hot Reload HTTP request has started.");
        var machine = await connecting.WaitAsync(deadline.Token);
        Console.WriteLine("The guest debugger handshake completed.");
        var start = machine.GetNextEventSet(180_000);
        Assert.Contains(start.Events, item => item.EventType == EventType.VMStart);
        machine.EnableEvents([EventType.UserBreak], SuspendPolicy.All);
        machine.Resume();
        var stopped = ProtocolAcceptance.UserBreak(machine);
        Console.WriteLine("The guest stopped in the baseline method.");
        var frame = Frame(stopped);
        Assert.True(stopped.Thread.GetFrames()[0].Method == frame.Method,
            "The editable method must be the active frame: " + string.Join(" -> ", stopped.Thread.GetFrames().Select(item => item.Method.FullName)));
        Assert.Equal(11, Value(frame));
        int statement = Array.FindIndex(File.ReadAllLines(source), line => line.Contains("value += 10;", StringComparison.Ordinal)) + 1;
        Assert.Contains(frame.Method.Locations, location => location.LineNumber == statement);
        var originalBody = frame.Method.GetMethodBody().GetILAsByteArray();
        var storage = frame.Method.DeclaringType.Assembly.GetType("Debugger.HotReload.Storage");
        Assert.Empty(storage.GetFields());
        Assert.DoesNotContain(frame.Method.DeclaringType.GetMethods(), method => method.Name == "Added");
        string baseline = Path.Combine(deltas, "Debugger.HotReload.Fixture.dll.1");
        byte[] metadata = await File.ReadAllBytesAsync(baseline + ".dmeta", deadline.Token);
        byte[] il = await File.ReadAllBytesAsync(baseline + ".dil", deadline.Token);
        byte[] symbols = await File.ReadAllBytesAsync(baseline + ".dpdb", deadline.Token);
        Assert.Throws<ArgumentException>(() => frame.Method.ReturnType.Assembly.ManifestModule.ApplyChanges(metadata, il, symbols));
        Assert.Equal(11, Value(frame));
        Console.WriteLine("Applying compiler deltas.");
        frame.Method.DeclaringType.Assembly.ManifestModule.ApplyChanges(metadata, il, symbols);
        Console.WriteLine("The runtime applied the deltas and refreshed debugger metadata.");
        Assert.Contains(storage.GetFields(), field => field.Name == "Added");
        Assert.Contains(frame.Method.DeclaringType.GetMethods(), method => method.Name == "Added");
        Assert.NotEqual(Convert.ToHexString(originalBody), Convert.ToHexString(frame.Method.GetMethodBody().GetILAsByteArray()));
        var destination = frame.Method.Locations.Single(location => location.LineNumber == statement + 1);
        stopped.Thread.SetIP(destination);
        machine.Resume();
        stopped = ProtocolAcceptance.UserBreak(machine);
        Assert.Equal(111, Value(Frame(stopped)));
        machine.Resume();
        Assert.Equal("111", await response);
        Console.WriteLine("The active interpreter frame executed updated IL and preserved its local value.");
        return 0;
    }

    private static Mono.Debugger.Soft.StackFrame Frame(Event stopped) => stopped.Thread.GetFrames()
        .Single(frame => frame.Method.DeclaringType.FullName == "Debugger.HotReload.Application" && frame.Method.Name == "Handle");

    private static int Value(Mono.Debugger.Soft.StackFrame frame) => (int)((PrimitiveValue)frame.GetValue(
        frame.GetVisibleVariables().Single(local => local.Name == "value"))).Value;
}
