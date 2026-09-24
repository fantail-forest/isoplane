using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Runtime.ExceptionServices;
using Mono.Debugger.Soft;
using Xunit;

namespace Isoplane.Debugging.Tests;

internal static class ProtocolAcceptance
{
    private const int DeadlineMs = 180_000;

    internal static async Task<int> Main(string[] arguments)
    {
        if (arguments is not [var endpoint, var url]) return 1;
        using var deadline = new CancellationTokenSource(DeadlineMs);
        using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp) { LingerState = new LingerOption(true, 0) };
        using var http = new HttpClient { Timeout = Timeout.InfiniteTimeSpan };
        var connecting = VirtualMachineManager.ConnectInternalAsync(socket, null, IPEndPoint.Parse(endpoint), null);
        var response = http.GetStringAsync(url, deadline.Token);
        var machine = await connecting.WaitAsync(deadline.Token);
        machine.EnableEvents([EventType.UserBreak], SuspendPolicy.All);
        var start = machine.GetNextEventSet(DeadlineMs);
        Assert.True(start?.Events.Any(value => value.EventType == EventType.VMStart) == true, "The real runtime must announce its initial suspension.");
        machine.Resume();
        var stopped = UserBreak(machine);
        var frame = stopped.Thread.GetFrames().First(value => value.Method.Name == "Handle");
        var locals = frame.GetVisibleVariables().ToDictionary(value => value.Name, frame.GetValue);
        var sample = (ObjectMirror)locals["sample"];
        var numbers = (ArrayMirror)locals["numbers"];
        var other = (ObjectMirror)locals["other"];
        var field = sample.Type.GetField("Value");
        var staticField = sample.Type.GetField("StaticValue");
        var literal = sample.Type.GetField("Constant");
        var threadStatic = sample.Type.GetField("ThreadValue");
        var target = new ManagedFieldBreakpoint(1, sample, field);
        machine.Suspend();
        machine.Resume();
        machine.SetManagedDataBreakpoints([target]);
        Reject(() => machine.SetManagedDataBreakpoints([target with { Id = 0 }]));
        Reject(() => machine.SetManagedDataBreakpoints([target, target]));
        Reject(() => machine.SetManagedDataBreakpoints([target with { Receiver = null }]));
        Reject(() => machine.SetManagedDataBreakpoints([target with { Receiver = other }]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedFieldBreakpoint(2, sample, staticField)]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedFieldBreakpoint(2, null, literal)]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedFieldBreakpoint(2, null, threadStatic)]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedArrayBreakpoint(2, numbers, -1)]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedArrayBreakpoint(2, numbers, numbers.Length)]));
        var wire = new ProtocolPackets(machine);
        Reject(() => wire.Send());
        Reject(() => wire.Send(("WriteByte", (byte)255)));
        Reject(() => wire.Send(("WriteByte", wire.Set)));
        Reject(() => wire.Send(("WriteByte", wire.Set), ("WriteInt", -1)));
        Reject(() => wire.Send(("WriteByte", wire.Set), ("WriteInt", 1)));
        Reject(() => wire.Send(("WriteByte", wire.Set), ("WriteInt", 0), ("WriteByte", (byte)0)));
        Reject(() => wire.Send(("WriteByte", wire.TakeHits), ("WriteByte", (byte)0)));
        Reject(() => wire.Location(255, Id(sample), Id(field)));
        Reject(() => wire.Location(wire.Field, Id(sample), 0));
        Reject(() => wire.Location(wire.Field, Id(sample), int.MaxValue));
        Reject(() => wire.Location(wire.Field, int.MaxValue, Id(field)));
        Reject(() => wire.Location(wire.Array, 0, 0));
        Reject(() => wire.Location(wire.Array, Id(sample), 0));
        machine.Resume();
        var changed = UserBreak(machine);
        Assert.True(machine.TakeManagedDataBreakpointHits().SequenceEqual([1]), "Rejected replacements must leave the original field watchpoint installed.");
        Assert.True(((PrimitiveValue)sample.GetValue(field)).Value is 9, "The retained watchpoint must stop on the actual managed write.");
        Assert.True(machine.TakeManagedDataBreakpointHits().Length == 0, "Reading watchpoint hits must consume the pending event identifiers.");
        machine.SetManagedDataBreakpoints([]);
        machine.Resume();
        Reject(() => wire.Send(("WriteByte", wire.Set), ("WriteInt", 0)));
        Reject(() => wire.Frames(changed.Thread.Id));
        machine.Suspend();
        var application = changed.Thread.GetFrames().First(value => value.Method.Name == "Handle").Method.DeclaringType;
        application.SetValue(application.GetField("ReleaseProtocolProbe"), machine.CreateValue(true));
        machine.Resume();
        Assert.True(await response == "9", "The guest must remain usable after every rejected protocol command.");
        var ended = machine.GetNextEventSet(DeadlineMs);
        if (ended?.Events.Any(value => value.EventType == EventType.VMDeath) == true)
            ended = machine.GetNextEventSet(DeadlineMs);
        Assert.True(ended?.Events.Any(value => value.EventType == EventType.VMDisconnect) == true,
            "The owned guest must finish closing its debugger transport after returning the HTTP response.");
        Console.WriteLine("Managed debugger protocol guards passed.");
        return 0;
    }

    private static Event UserBreak(VirtualMachine machine)
    {
        var events = machine.GetNextEventSet(DeadlineMs);
        Assert.True(events?.Events.Any(value => value.EventType == EventType.UserBreak) == true, "The real guest must stop at the requested managed location.");
        return events!.Events.First(value => value.EventType == EventType.UserBreak);
    }

    private static void Reject(Action command)
    {
        var error = Record.Exception(command);
        Assert.True(error is CommandException or ObjectCollectedException or VMNotSuspendedException,
            $"The runtime must reject invalid managed watchpoint input without ending the session; received {error}.");
    }

    private static long Id(Mirror mirror) => (long)typeof(Mirror).GetProperty("Id", BindingFlags.Instance | BindingFlags.NonPublic)!.GetValue(mirror)!;

    // Exercise malformed payloads through the backend's codec and transport, without duplicating SDB framing.
    private sealed class ProtocolPackets
    {
        private readonly object connection;
        private readonly Type packetType = typeof(Connection).GetNestedType("PacketWriter", BindingFlags.NonPublic)!;
        private readonly object commandSet;
        private readonly MethodInfo send;
        private readonly int command;
        internal byte Set { get; } = Constant<byte>("ManagedDataBreakpointSet");
        internal byte TakeHits { get; } = Constant<byte>("ManagedDataBreakpointTakeHits");
        internal byte Field { get; } = Constant<byte>("ManagedDataBreakpointField");
        internal byte Array { get; } = Constant<byte>("ManagedDataBreakpointArray");

        internal ProtocolPackets(VirtualMachine machine)
        {
            connection = typeof(VirtualMachine).GetField("conn", BindingFlags.Instance | BindingFlags.NonPublic)!.GetValue(machine)!;
            var setType = typeof(Connection).GetNestedType("CommandSet", BindingFlags.NonPublic)!;
            commandSet = Enum.Parse(setType, "VM");
            command = Constant<int>("ManagedDataBreakpointCommand");
            send = typeof(Connection).GetMethod("SendReceive", BindingFlags.Instance | BindingFlags.NonPublic, [setType, typeof(int), packetType])!;
        }

        internal void Location(byte kind, long owner, long member) => Send(
            ("WriteByte", Set), ("WriteInt", 1), ("WriteInt", 2),
            ("WriteByte", kind), ("WriteId", owner), ("WriteId", member));

        internal void Frames(long thread) => Exchange(
            Enum.Parse(typeof(Connection).GetNestedType("CommandSet", BindingFlags.NonPublic)!, "THREAD"),
            (int)Enum.Parse(typeof(Connection).GetNestedType("CmdThread", BindingFlags.NonPublic)!, "GET_FRAME_INFO"),
            [("WriteId", thread), ("WriteInt", 0), ("WriteInt", -1)]);

        internal void Send(params (string Method, object Value)[] fields) => Exchange(commandSet, command, fields);

        private void Exchange(object set, int operation, (string Method, object Value)[] fields)
        {
            var packet = Activator.CreateInstance(packetType, connection)!;
            foreach (var (method, value) in fields) packetType.GetMethod(method, [value.GetType()])!.Invoke(packet, [value]);
            try { send.Invoke(connection, [set, operation, packet]); }
            catch (TargetInvocationException error) when (error.InnerException is not null)
            {
                ExceptionDispatchInfo.Capture(error.InnerException).Throw();
            }
        }

        private static T Constant<T>(string name) => (T)typeof(Connection).GetField(name, BindingFlags.Static | BindingFlags.NonPublic)!.GetRawConstantValue()!;
    }
}
