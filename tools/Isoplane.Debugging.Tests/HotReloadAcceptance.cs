using System.Net;
using System.Net.Sockets;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using Mono.Debugger.Soft;
using Xunit;
using Isoplane.Build;

namespace Isoplane.Debugging.Tests;

internal static class HotReloadAcceptance
{
    internal static async Task<int> OpenCompilerAsync(string project, string package, string artifacts)
    {
        using var deadline = new CancellationTokenSource(TimeSpan.FromMinutes(3));
        using var baseline = HotReloadBaseline.Read(package);
        string source = Path.Combine(Path.GetDirectoryName(project)!, "..", "hotreload.cs");
        using var image = File.OpenRead(baseline.Assemblies["Debugger.HotReload.Fixture"]);
        using var pe = new PEReader(image);
        var metadata = pe.GetMetadataReader();
        Guid module = metadata.GetGuid(metadata.GetModuleDefinition().Mvid);
        string original = await File.ReadAllTextAsync(source, deadline.Token);
        string edited = original.Replace("value += 10; // HOT_RELOAD_BODY", "value += 20; // HOT_RELOAD_BODY", StringComparison.Ordinal);
        Assert.NotEqual(original, edited);
        await using (var compiler = await HotReloadCompiler.StartAsync(project,
            new() { ["Configuration"] = "Release", ["ArtifactsPath"] = artifacts }, baseline.Assemblies, [], deadline.Token))
        {
            Assert.Contains(compiler.Documents, document => Path.GetFileName(document) == "hotreload.cs");
            var unchanged = await compiler.SendAsync(new("update", new([], [], [], [new(source, original)])), deadline.Token);
            Assert.Empty(unchanged.Diagnostics);
            Assert.Empty(unchanged.Updates);
            Assert.False(unchanged.Pending);
            var update = await compiler.SendAsync(new("update", new([], [module], ["Baseline"], [new(source, edited)])), deadline.Token);
            foreach (string diagnostic in update.Diagnostics) Console.Error.WriteLine(diagnostic);
            Assert.Empty(update.Diagnostics);
            Assert.False(update.RestartRequired);
            Assert.Equal(module, Assert.Single(update.Updates).Module);
            Assert.True(update.Pending);
            await compiler.SendAsync(new("discard"), deadline.Token);
        }
        await File.WriteAllTextAsync(Path.ChangeExtension(baseline.Assemblies["Debugger.HotReload.Fixture"], ".pdb"),
            "corrupt owned symbols", deadline.Token);
        await using (var compiler = await HotReloadCompiler.StartAsync(project,
            new() { ["Configuration"] = "Release", ["ArtifactsPath"] = artifacts }, baseline.Assemblies, [], deadline.Token))
        {
            var update = await compiler.SendAsync(new("update", new([], [module], ["Baseline"], [new(source, edited)])), deadline.Token);
            Assert.NotEmpty(update.Diagnostics);
            Assert.Empty(update.Updates);
            await compiler.SendAsync(new("discard"), deadline.Token);
        }
        Console.WriteLine("The compiler emitted an update from its owned baseline and rejected corrupt owned symbols.");
        return 0;
    }

    internal static async Task<int> RunAsync(string endpoint, string url, string deltas, string source, bool withoutSymbols = false)
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
        var (transport, package) = await owner.ConnectAsync(IPEndPoint.Parse(endpoint)).WaitAsync(deadline.Token);
        Console.WriteLine("The debug session allocated its guest transport.");
        var connecting = Task.Run(() => VirtualMachineManager.ConnectInternalAsync(socket, null, transport, null), deadline.Token);
        var response = http.GetStringAsync(url, deadline.Token);
        Console.WriteLine("The Hot Reload HTTP request has started.");
        var machine = await ProtocolAcceptance.WaitForConnectionAsync(connecting, response, deadline.Token);
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
        Assert.True(frame.CanRemap, "A completed debugger break is a supported remapping boundary.");
        Assert.All(stopped.Thread.GetFrames().Skip(1), caller => Assert.False(caller.CanRemap));
        Assert.Equal(11, Value(frame));
        int statement = SourceLine(source, scenario.Marker);
        Assert.Contains(frame.Method.Locations, location => location.LineNumber == statement);
        Assert.Equal(1, frame.MethodVersion);
        int originalLine = frame.Location.LineNumber;
        var originalLocations = frame.Method.Locations.ToDictionary(point => point.ILOffset, point => point.LineNumber);
        var originalBody = frame.Method.GetMethodBody().GetILAsByteArray();
        var activeFlow = StatementFlow.Capture(frame.Method.GetMethodBody());
        int activeOffset = frame.ILOffset;
        var assembly = frame.Method.DeclaringType.Assembly;
        using var compilerBaseline = scenario.Method != "HandleLayout" && !withoutSymbols ? HotReloadBaseline.Read(package) : null;
        await using var compiler = compilerBaseline is not null ? await HotReloadCompiler.StartAsync(
            Path.Combine(Path.GetDirectoryName(source)!, "hotreload", "HotReloadFixture.csproj"),
            new() { ["Configuration"] = "Release", ["ArtifactsPath"] = Path.GetFullPath("../../..", deltas) },
            compilerBaseline.Assemblies,
            machine.GetEnCCapabilities().Split(' ', StringSplitOptions.RemoveEmptyEntries), deadline.Token) : null;
        var application = assembly.GetType("Debugger.HotReload.Application");
        var storage = assembly.GetType("Debugger.HotReload.Storage");
        Assert.Empty(storage.GetFields());
        Assert.DoesNotContain(application.GetMethods(), method => method.Name == "Added");
        string baseline = Path.Combine(deltas, "Debugger.HotReload.Fixture.dll.1");
        byte[] metadata = await File.ReadAllBytesAsync(baseline + ".dmeta", deadline.Token);
        byte[] il = await File.ReadAllBytesAsync(baseline + ".dil", deadline.Token);
        byte[] symbols = withoutSymbols ? [] : await File.ReadAllBytesAsync(baseline + ".dpdb", deadline.Token);
        ActiveStatementUpdate[] remappedStatements = [];
        if (compiler is not null)
        {
            var update = await CompileAsync(compiler, frame, source, "updated.cs.in", deadline.Token);
            metadata = update.Metadata;
            il = update.Il;
            symbols = update.Symbols;
            remappedStatements = update.ActiveStatements;
        }
        Assert.Throws<ArgumentException>(() => frame.Method.ReturnType.Assembly.ManifestModule.ApplyChanges(metadata, il, symbols));
        Assert.Equal(11, Value(frame));
        byte[] futureMetadata = await File.ReadAllBytesAsync(Path.Combine(deltas, "Debugger.HotReload.Fixture.dll.2.dmeta"), deadline.Token);
        Assert.Throws<ArgumentException>(() => assembly.ManifestModule.ApplyChanges(futureMetadata, il, symbols));
        Console.WriteLine("Applying compiler deltas.");
        assembly.ManifestModule.ApplyChanges(metadata, il, symbols);
        if (compiler is not null) await compiler.SendAsync(new("commit"), deadline.Token);
        Assert.Throws<ArgumentException>(() => assembly.ManifestModule.ApplyChanges(metadata, il, symbols));
        Console.WriteLine("The runtime applied the deltas and refreshed debugger metadata.");
        frame.ClearDebugInfoToTryToGetFromLoadedPdb();
        Assert.Equal(1, frame.MethodVersion);
        Assert.Equal(originalLine, frame.Location.LineNumber);
        Assert.Equal(originalBody, frame.GetMethodBody().GetILAsByteArray());
        Assert.Contains(storage.GetFields(), field => field.Name == "Added");
        var added = Assert.Single(application.GetMethods(), method => method.Name == "Added");
        if (withoutSymbols)
        {
            Assert.Empty(added.Locations);
            Assert.Equal(11, Value(frame));
            machine.Resume();
            Assert.Equal("11", await response);
            return 0;
        }
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
            var retainedLocal = frame.GetVisibleVariableByName("value");
            Assert.Equal("System.Int32", retainedLocal.Type.FullName);
            Assert.Equal(11, ((PrimitiveValue)frame.GetValue(retainedLocal)).Value);
            frame.SetValue(retainedLocal, machine.CreateValue(17));
            Assert.Equal(17, ((PrimitiveValue)frame.GetValue(retainedLocal)).Value);
            Assert.Throws<ArgumentException>(() => stopped.Thread.SetIP(destination));
            var step = machine.CreateStepRequest(stopped.Thread);
            step.Depth = StepDepth.Over;
            step.Size = StepSize.Line;
            step.Enable();
            machine.Resume();
            var stepped = Assert.Single(machine.GetNextEventSet(180_000).Events, item => item.EventType == EventType.Step);
            step.Disable();
            var originalFrame = Frame(stepped, scenario);
            Assert.Equal(1, originalFrame.MethodVersion);
            Assert.True(originalFrame.ILOffset > activeOffset, "Step Over must advance within the retained invocation.");
            Assert.Equal(originalLocations[originalFrame.ILOffset], originalFrame.LineNumber);
            Assert.Equal(17, Value(originalFrame));
            machine.Resume();
            Assert.Equal("17", await response);
            return 0;
        }
        if (scenario.Method == "Handle")
        {
            var remap = Assert.Single(remappedStatements);
            Assert.Equal(frame.Method.MetadataToken, remap.MethodToken);
            Assert.Equal(frame.MethodVersion, remap.MethodVersion);
            Assert.Equal(frame.ILOffset, remap.IlOffset);
            Assert.Equal(originalLine, remap.NewSpan.StartLine);
            var editedStatement = Assert.Single(frame.Method.Locations, point => point.LineNumber == remap.NewSpan.StartLine + 1
                && point.ColumnNumber == remap.NewSpan.StartColumn + 1);
            stopped.Thread.SetIP(editedStatement);
            Assert.Equal(2, Frame(stopped, scenario).MethodVersion);
            Assert.Equal(11, Value(Frame(stopped, scenario)));
        }
        stopped.Thread.SetIP(destination);
        Assert.Equal(scenario.Method == "HandleUnchanged" ? 1 : 2, Frame(stopped, scenario).MethodVersion);
        if (scenario.Method == "Handle")
        {
            var math = Assert.IsType<TypeMirror>(machine.RootDomain.Corlib.GetType("System.Math"));
            var absolute = math.GetMethods().Single(method => method.Name == "Abs"
                && method.GetParameters() is [var argument] && argument.ParameterType.FullName == "System.Int32");
            var evaluated = await math.InvokeMethodAsync(stopped.Thread, absolute, [machine.CreateValue(-7)],
                InvokeOptions.SingleThreaded | InvokeOptions.DisableBreakpoints).WaitAsync(deadline.Token);
            Assert.Equal(7, Assert.IsType<PrimitiveValue>(evaluated).Value);
            Assert.Equal(destination.ILOffset, Frame(stopped, scenario).ILOffset);
        }
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
            : await CompileAsync(compiler, frame, source, "updated-again.cs.in", deadline.Token);
        assembly.ManifestModule.ApplyChanges(second.Metadata, second.Il, second.Symbols);
        if (compiler is not null) await compiler.SendAsync(new("commit"), deadline.Token);
        updatedLine = SourceLine(Path.ChangeExtension(source, "updated-again.cs.in"), scenario.Marker);
        destination = frame.Method.Locations.Single(location => location.LineNumber == updatedLine);
        updatedFlow = StatementFlow.Capture(frame.Method.GetMethodBody());
        Assert.Contains(destination.ILOffset, StatementFlow.ReachableTargets(activeFlow, updatedFlow, activeOffset));
        stopped.Thread.SetIP(destination);
        Assert.Equal(scenario.Method switch { "HandleUnchanged" => 1, "Handle" => 2, _ => 3 },
            Frame(stopped, scenario).MethodVersion);
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
        string source, string update, CancellationToken cancellation)
    {
        var location = frame.Location;
        Guid module = frame.Method.DeclaringType.Assembly.ManifestModule.ModuleVersionId;
        var statement = new ActiveStatement(module, frame.Method.MetadataToken, frame.MethodVersion, frame.ILOffset, location.SourceFile,
            location.LineNumber - 1, location.ColumnNumber - 1, location.EndLineNumber - 1, location.EndColumnNumber - 1, true, frame.IsCurrentMethodVersion);
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
