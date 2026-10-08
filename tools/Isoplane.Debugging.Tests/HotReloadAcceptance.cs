using System.Net;
using System.Net.Sockets;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using Mono.Debugger.Soft;
using Xunit;
using Isoplane.Build;

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
        await using var compiler = scenario.Method != "HandleLayout" ? await HotReloadCompiler.StartAsync(
            Path.Combine(Path.GetDirectoryName(source)!, "hotreload", "HotReloadFixture.csproj"),
            new() { ["Configuration"] = "Release", ["ArtifactsPath"] = Path.GetFullPath("../../..", deltas) },
            new() { ["Debugger.HotReload.Fixture"] = Path.Combine(deltas, "Debugger.HotReload.Fixture.dll") },
            machine.GetEnCCapabilities().Split(' ', StringSplitOptions.RemoveEmptyEntries), deadline.Token) : null;
        var application = assembly.GetType("Debugger.HotReload.Application");
        var storage = assembly.GetType("Debugger.HotReload.Storage");
        Assert.Empty(storage.GetFields());
        Assert.DoesNotContain(application.GetMethods(), method => method.Name == "Added");
        string baseline = Path.Combine(deltas, "Debugger.HotReload.Fixture.dll.1");
        byte[] metadata = await File.ReadAllBytesAsync(baseline + ".dmeta", deadline.Token);
        byte[] il = await File.ReadAllBytesAsync(baseline + ".dil", deadline.Token);
        byte[] symbols = await File.ReadAllBytesAsync(baseline + ".dpdb", deadline.Token);
        if (compiler is not null)
        {
            var update = await CompileAsync(compiler, frame, source, "updated.cs.in", 1, deadline.Token);
            metadata = update.Metadata;
            il = update.Il;
            symbols = update.Symbols;
        }
        Assert.Throws<ArgumentException>(() => frame.Method.ReturnType.Assembly.ManifestModule.ApplyChanges(metadata, il, symbols));
        Assert.Equal(11, Value(frame));
        byte[] futureMetadata = await File.ReadAllBytesAsync(Path.Combine(deltas, "Debugger.HotReload.Fixture.dll.2.dmeta"), deadline.Token);
        Assert.Throws<ArgumentException>(() => assembly.ManifestModule.ApplyChanges(futureMetadata, il, symbols));
        Console.WriteLine("Applying compiler deltas.");
        int nextVersion = UpdatesMethod(metadata, frame.Method.MetadataToken) ? 2 : 1;
        assembly.ManifestModule.ApplyChanges(metadata, il, symbols);
        if (compiler is not null) await compiler.SendAsync(new("commit"), deadline.Token);
        Assert.Throws<ArgumentException>(() => assembly.ManifestModule.ApplyChanges(metadata, il, symbols));
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
        frame = Frame(stopped, scenario);
        Assert.Equal(scenario.UpdatedValue, Value(frame));
        activeFlow = StatementFlow.Capture(frame.Method.GetMethodBody());
        activeOffset = frame.ILOffset;
        baseline = Path.Combine(deltas, "Debugger.HotReload.Fixture.dll.2");
        var second = compiler is null ? new ModuleUpdate(assembly.ManifestModule.ModuleVersionId,
            await File.ReadAllBytesAsync(baseline + ".dmeta", deadline.Token),
            await File.ReadAllBytesAsync(baseline + ".dil", deadline.Token),
            await File.ReadAllBytesAsync(baseline + ".dpdb", deadline.Token))
            : await CompileAsync(compiler, frame, source, "updated-again.cs.in", nextVersion, deadline.Token);
        assembly.ManifestModule.ApplyChanges(second.Metadata, second.Il, second.Symbols);
        if (compiler is not null) await compiler.SendAsync(new("commit"), deadline.Token);
        updatedLine = SourceLine(Path.ChangeExtension(source, "updated-again.cs.in"), scenario.Marker);
        destination = frame.Method.Locations.Single(location => location.LineNumber == updatedLine);
        updatedFlow = StatementFlow.Capture(frame.Method.GetMethodBody());
        Assert.Contains(destination.ILOffset, StatementFlow.ReachableTargets(activeFlow, updatedFlow, activeOffset));
        stopped.Thread.SetIP(destination);
        machine.Resume();
        stopped = ProtocolAcceptance.UserBreak(machine);
        int increment = scenario.Method == "HandleUnchanged" ? 10 : 200;
        Assert.Equal(scenario.UpdatedValue + increment, Value(Frame(stopped, scenario)));
        machine.Resume();
        Assert.Equal((int.Parse(scenario.Response, System.Globalization.CultureInfo.InvariantCulture) + increment)
            .ToString(System.Globalization.CultureInfo.InvariantCulture), await response);
        Console.WriteLine("The active interpreter frame executed updated IL and preserved its local value.");
        return 0;
    }

    private static async Task<ModuleUpdate> CompileAsync(HotReloadCompiler compiler, Mono.Debugger.Soft.StackFrame frame,
        string source, string update, int version, CancellationToken cancellation)
    {
        var location = frame.Location;
        Guid module = frame.Method.DeclaringType.Assembly.ManifestModule.ModuleVersionId;
        var statement = new ActiveStatement(module, frame.Method.MetadataToken, version, frame.ILOffset, location.SourceFile,
            location.LineNumber - 1, location.ColumnNumber - 1, location.EndLineNumber - 1, location.EndColumnNumber - 1, true, true);
        var edit = new CompilerEdit([statement], [module], frame.VirtualMachine.GetEnCCapabilities().Split(' ', StringSplitOptions.RemoveEmptyEntries),
            [new(source, await File.ReadAllTextAsync(Path.ChangeExtension(source, update), cancellation))]);
        if (update == "updated.cs.in")
        {
            string original = await File.ReadAllTextAsync(source, cancellation);
            var unchanged = await compiler.SendAsync(new("update", edit with { Sources = [new(source, original)] }), cancellation);
            Assert.Empty(unchanged.Updates);
            Assert.Empty(unchanged.Diagnostics);
            Assert.False(unchanged.Pending);
            var invalid = await compiler.SendAsync(new("update", edit with { Sources = [new(source, original + "\n#error deliberate-edit-error\n")] }), cancellation);
            Assert.Empty(invalid.Updates);
            Assert.NotEmpty(invalid.Diagnostics);
            Assert.False(invalid.Pending);
        }
        if (update == "updated.cs.in")
        {
            var discarded = await compiler.SendAsync(new("update", edit), cancellation);
            Assert.True(discarded.Pending);
            Assert.Single(discarded.Updates);
            await compiler.SendAsync(new("discard"), cancellation);
        }
        var reply = await compiler.SendAsync(new("update", edit), cancellation);
        Assert.Empty(reply.Diagnostics);
        Assert.False(reply.RestartRequired);
        Assert.True(reply.Pending);
        var result = Assert.Single(reply.Updates);
        Assert.Equal(module, result.Module);
        return result;
    }

    private static bool UpdatesMethod(byte[] metadata, int token)
    {
        using var stream = new MemoryStream(metadata, writable: false);
        using var provider = MetadataReaderProvider.FromMetadataStream(stream);
        return provider.GetMetadataReader().GetEditAndContinueMapEntries()
            .Any(handle => handle.Kind == HandleKind.MethodDefinition && MetadataTokens.GetToken(handle) == token);
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
