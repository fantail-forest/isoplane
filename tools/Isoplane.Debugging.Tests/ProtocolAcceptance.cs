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

    internal static async Task<VirtualMachine> WaitForConnectionAsync(Task<VirtualMachine> connecting,
        Task<string> response, CancellationToken cancellation)
    {
        if (await Task.WhenAny(connecting, response).WaitAsync(cancellation) == response)
        {
            string body = await response;
            throw new InvalidOperationException($"The guest request completed before its debugger handshake: {body}");
        }
        return await connecting;
    }

    internal static async Task<int> Main(string[] arguments)
    {
        if (arguments is ["--compiler", var compilerProject, var compilerPackage, var compilerArtifacts])
            return await HotReloadAcceptance.OpenCompilerAsync(compilerProject, compilerPackage, compilerArtifacts);
        if (arguments is [var noSymbolsEndpoint, var noSymbolsUrl, var noSymbolsDeltas, var noSymbolsSource, "--without-symbols"])
            return await HotReloadAcceptance.RunAsync(noSymbolsEndpoint, noSymbolsUrl, noSymbolsDeltas, noSymbolsSource, withoutSymbols: true);
        if (arguments is [var hotReloadEndpoint, var hotReloadUrl, var deltas, var source])
            return await HotReloadAcceptance.RunAsync(hotReloadEndpoint, hotReloadUrl, deltas, source);
        if (arguments is not [var endpoint, var url]) return 1;
        using var deadline = new CancellationTokenSource(DeadlineMs);
        using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp) { LingerState = new LingerOption(true, 0) };
        using var cancelTransport = deadline.Token.Register(socket.Dispose);
        using var http = new HttpClient { Timeout = Timeout.InfiniteTimeSpan };
        await using var owner = new Isoplane.Debugging.GuestSession();
        var (transportEndpoint, _) = await owner.ConnectAsync(IPEndPoint.Parse(endpoint)).WaitAsync(deadline.Token);
        var connecting = Task.Run(() => VirtualMachineManager.ConnectInternalAsync(socket, null, transportEndpoint, null), deadline.Token);
        var response = http.GetStringAsync(url, deadline.Token);
        var machine = await WaitForConnectionAsync(connecting, response, deadline.Token);
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
        var nested = sample.Type.GetField("Nested");
        var leaf = nested.FieldType.GetField("Leaf");
        var leafValue = leaf.FieldType.GetField("Value");
        var leafName = leaf.FieldType.GetField("Name");
        var nestedTarget = new ManagedFieldPathBreakpoint(2, new(2, sample, nested), [leaf, leafValue]);
        var referenceTarget = new ManagedFieldPathBreakpoint(3, new(3, sample, nested), [leaf, leafName]);
        machine.Suspend();
        machine.Resume();
        machine.SetManagedDataBreakpoints([target, nestedTarget, referenceTarget]);
        Reject(() => machine.SetManagedDataBreakpoints([target with { Id = 0 }]));
        Reject(() => machine.SetManagedDataBreakpoints([target, target]));
        Reject(() => machine.SetManagedDataBreakpoints([target with { Receiver = null }]));
        Reject(() => machine.SetManagedDataBreakpoints([target with { Receiver = other }]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedFieldBreakpoint(2, sample, staticField)]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedFieldBreakpoint(2, null, literal)]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedFieldBreakpoint(2, null, threadStatic)]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedArrayBreakpoint(2, numbers, -1)]));
        Reject(() => machine.SetManagedDataBreakpoints([new ManagedArrayBreakpoint(2, numbers, numbers.Length)]));
        Reject(() => machine.SetManagedDataBreakpoints([nestedTarget with { Fields = [] }]));
        Reject(() => machine.SetManagedDataBreakpoints([nestedTarget with { Fields = [field] }]));
        Reject(() => machine.SetManagedDataBreakpoints([nestedTarget with { Fields = [staticField] }]));
        Reject(() => machine.SetManagedDataBreakpoints([nestedTarget with { Fields = [leaf, leafName, leafValue] }]));
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
        Reject(() => wire.Location(wire.FieldPath, Id(sample), Id(nested)));
        Reject(() => wire.FieldPathLocation(Id(sample), Id(nested), -1));
        Reject(() => wire.FieldPathLocation(Id(sample), Id(nested), int.MaxValue));
        Reject(() => wire.FieldPathLocation(Id(sample), Id(nested), 1, 0));
        Reject(() => wire.FieldPathLocation(Id(sample), Id(nested), 1, int.MaxValue));
        Reject(() => wire.Location(wire.Array, 0, 0));
        Reject(() => wire.Location(wire.Array, Id(sample), 0));
        Reject(() => wire.Location(wire.Local, Id(stopped.Thread), Id(frame)));
        Reject(() => wire.LocalLocation(0, Id(frame), 0));
        Reject(() => wire.LocalLocation(Id(sample), Id(frame), 0));
        Assert.Throws<InvalidStackFrameException>(() => wire.LocalLocation(Id(stopped.Thread), 0, 0));
        Assert.Throws<InvalidStackFrameException>(() => wire.LocalLocation(Id(stopped.Thread), int.MaxValue, 0));
        Reject(() => wire.LocalLocation(Id(stopped.Thread), Id(frame), -1));
        Reject(() => wire.LocalLocation(Id(stopped.Thread), Id(frame), int.MaxValue));
        var debuggerOwned = machine.RootDomain.CreateByteArray([3, 1, 4]);
        machine.Suspend();
        machine.Resume();
        var gc = Assert.IsType<TypeMirror>(machine.RootDomain.Corlib.GetType("System.GC"));
        var collect = gc.GetMethods().Single(method => method.Name == "Collect" && method.GetParameters().Length == 0);
        foreach (var options in new[] { InvokeOptions.DisableBreakpoints, InvokeOptions.SingleThreaded | InvokeOptions.DisableBreakpoints })
        {
            await gc.InvokeMethodAsync(stopped.Thread, collect, [], options).WaitAsync(deadline.Token);
            Assert.False(debuggerOwned.IsCollected, "Debugger-created values must survive function evaluation and nested suspension until the guest resumes.");
        }
        Assert.Equal(new byte[] { 3, 1, 4 }, debuggerOwned.GetValues(0, 3).Cast<PrimitiveValue>().Select(value => (byte)value.Value));
        machine.Resume();
        var changed = UserBreak(machine);
        var events = machine.TakeManagedDataBreakpointHits();
        Assert.Empty(events.Expired);
        Assert.True(events.Hits.SequenceEqual([1]), "Rejected replacements must leave the original field watchpoint installed.");
        Assert.True(((PrimitiveValue)sample.GetValue(field)).Value is 9, "The retained watchpoint must stop on the actual managed write.");
        Assert.True(machine.TakeManagedDataBreakpointHits().Hits.Length == 0, "Reading watchpoint hits must consume the pending event identifiers.");
        foreach (int id in new[] { 2, 3 })
        {
            machine.Resume();
            changed = UserBreak(machine);
            Assert.Equal(new[] { id }, machine.TakeManagedDataBreakpointHits().Hits);
        }
        var nestedValue = (StructMirror)((StructMirror)sample.GetValue(nested))[leaf.Name];
        Assert.Equal(11, ((PrimitiveValue)nestedValue[leafValue.Name]).Value);
        Assert.Equal("after", ((StringMirror)nestedValue[leafName.Name]).Value);
        machine.SetManagedDataBreakpoints([]);
        machine.Resume();
        Reject(() => wire.Send(("WriteByte", wire.Set), ("WriteInt", 0)));
        Reject(() => wire.Frames(changed.Thread.Id));
        Reject(() => changed.Thread.GetFrames());
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

    internal static Event UserBreak(VirtualMachine machine)
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
        internal byte Local { get; } = Constant<byte>("ManagedDataBreakpointLocal");
        internal byte FieldPath { get; } = Constant<byte>("ManagedDataBreakpointFieldPath");

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

        internal void LocalLocation(long thread, long frame, int index) => Send(
            ("WriteByte", Set), ("WriteInt", 1), ("WriteInt", 4),
            ("WriteByte", Local), ("WriteId", thread), ("WriteId", frame), ("WriteInt", index));

        internal void FieldPathLocation(long owner, long field, int count, params long[] path) => Send(
            new (string Method, object Value)[] {
                ("WriteByte", Set), ("WriteInt", 1), ("WriteInt", 2),
                ("WriteByte", FieldPath), ("WriteId", owner), ("WriteId", field), ("WriteInt", count)
            }.Concat(path.Select(id => ("WriteId", (object)id))).ToArray());

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
