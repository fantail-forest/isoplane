using System.Net;
using System.Net.Sockets;
using Mono.Debugger.Soft;
using Xunit;

namespace Isoplane.Debugging.Tests;

internal static class HotReloadAcceptance
{
    internal static async Task<int> RunAsync(string endpoint, string url, string deltas, string source)
    {
        var scenario = new Uri(url).AbsolutePath switch
        {
            "/hotreload" => new Scenario("Handle", "HOT_RELOAD_BODY", 111, "111"),
            "/hotreload-layout" => new Scenario("HandleLayout", "HOT_RELOAD_LAYOUT", 11, "11"),
            "/hotreload-try" => new Scenario("HandleTry", "HOT_RELOAD_TRY", 111, "1111"),
            "/hotreload-async" => new Scenario("HandleAsync", "HOT_RELOAD_AWAIT", 111, "111"),
            "/hotreload-unchanged-handler" => new Scenario("HandleUnchanged", "HOT_RELOAD_UNCHANGED", 21, "21"),
            _ => throw new ArgumentException("Unknown Hot Reload fixture route.", nameof(url))
        };
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
        var frame = Frame(stopped, scenario);
        Assert.True(stopped.Thread.GetFrames()[0].Method == frame.Method,
            "The editable method must be the active frame: " + string.Join(" -> ", stopped.Thread.GetFrames().Select(item => item.Method.FullName)));
        Assert.Equal(11, Value(frame));
        int statement = SourceLine(source, scenario.Marker);
        Assert.Contains(frame.Method.Locations, location => location.LineNumber == statement);
        var originalBody = frame.Method.GetMethodBody().GetILAsByteArray();
        var activeFlow = StatementFlow.Capture(frame.Method.GetMethodBody());
        int activeOffset = frame.ILOffset;
        var assembly = frame.Method.DeclaringType.Assembly;
        var application = assembly.GetType("Debugger.HotReload.Application");
        var storage = assembly.GetType("Debugger.HotReload.Storage");
        Assert.Empty(storage.GetFields());
        Assert.DoesNotContain(application.GetMethods(), method => method.Name == "Added");
        string baseline = Path.Combine(deltas, "Debugger.HotReload.Fixture.dll.1");
        byte[] metadata = await File.ReadAllBytesAsync(baseline + ".dmeta", deadline.Token);
        byte[] il = await File.ReadAllBytesAsync(baseline + ".dil", deadline.Token);
        byte[] symbols = await File.ReadAllBytesAsync(baseline + ".dpdb", deadline.Token);
        Assert.Throws<ArgumentException>(() => frame.Method.ReturnType.Assembly.ManifestModule.ApplyChanges(metadata, il, symbols));
        Assert.Equal(11, Value(frame));
        Console.WriteLine("Applying compiler deltas.");
        assembly.ManifestModule.ApplyChanges(metadata, il, symbols);
        Console.WriteLine("The runtime applied the deltas and refreshed debugger metadata.");
        Assert.Contains(storage.GetFields(), field => field.Name == "Added");
        var added = Assert.Single(application.GetMethods(), method => method.Name == "Added");
        Assert.True(StatementFlow.CanMove(added));
        var addedFlow = StatementFlow.Capture(added.GetMethodBody());
        Assert.Contains(added.Locations[0].ILOffset, StatementFlow.ReachableTargets(addedFlow, addedFlow, added.Locations[0].ILOffset));
        string currentBody = Convert.ToHexString(frame.Method.GetMethodBody().GetILAsByteArray());
        if (scenario.Method == "HandleUnchanged") Assert.Equal(Convert.ToHexString(originalBody), currentBody);
        else Assert.NotEqual(Convert.ToHexString(originalBody), currentBody);
        int updatedLine = SourceLine(Path.ChangeExtension(source, "updated.cs.in"), scenario.Marker);
        var destination = frame.Method.Locations.Single(location => location.LineNumber == updatedLine);
        var updatedFlow = StatementFlow.Capture(frame.Method.GetMethodBody());
        Assert.Contains(destination.ILOffset, StatementFlow.ReachableTargets(activeFlow, updatedFlow, activeOffset));
        if (scenario.Method == "HandleLayout")
        {
            Assert.Throws<ArgumentException>(() => stopped.Thread.SetIP(destination));
            machine.Resume();
            Assert.Equal(scenario.Response, await response);
            return 0;
        }
        stopped.Thread.SetIP(destination);
        machine.Resume();
        var nextStop = machine.GetNextEventSet(180_000);
        Assert.Equal(SuspendPolicy.All, nextStop.SuspendPolicy);
        Console.WriteLine("The edited method stopped with suspension policy " + nextStop.SuspendPolicy);
        stopped = Assert.Single(nextStop.Events, item => item.EventType == EventType.UserBreak);
        Assert.Equal(scenario.UpdatedValue, Value(Frame(stopped, scenario)));
        machine.Resume();
        Assert.Equal(scenario.Response, await response);
        Console.WriteLine("The active interpreter frame executed updated IL and preserved its local value.");
        return 0;
    }

    private sealed record Scenario(string Method, string Marker, int UpdatedValue, string Response);

    private static int SourceLine(string path, string marker)
    {
        int line = Array.FindIndex(File.ReadAllLines(path), text => text.Contains(marker, StringComparison.Ordinal));
        Assert.True(line >= 0, $"The fixture {path} must contain {marker}.");
        return line + 1;
    }

    private static Mono.Debugger.Soft.StackFrame Frame(Event stopped, Scenario scenario) => stopped.Thread.GetFrames()
        .Single(frame => scenario.Method == "HandleAsync"
            ? frame.Method.Name == "MoveNext" && frame.Method.DeclaringType.Name.StartsWith("<HandleAsync>", StringComparison.Ordinal)
            : frame.Method.DeclaringType.FullName == "Debugger.HotReload.Application" && frame.Method.Name == scenario.Method);

    private static int Value(Mono.Debugger.Soft.StackFrame frame)
    {
        if (frame.Method.Name != "MoveNext")
            return (int)((PrimitiveValue)frame.GetValue(frame.GetVisibleVariables().Single(local => local.Name == "value"))).Value;
        var field = frame.Method.DeclaringType.GetFields().Single(candidate => candidate.Name.StartsWith("<value>", StringComparison.Ordinal));
        var value = frame.GetThis() switch
        {
            ObjectMirror receiver => receiver.GetValue(field),
            StructMirror receiver => receiver[field.Name],
            _ => throw new InvalidOperationException("The async frame must expose its state machine.")
        };
        return (int)((PrimitiveValue)value).Value;
    }
}
