using System.Collections;
using System.Collections.Immutable;
using System.Linq.Expressions;
using System.Reflection;
using Isoplane.Build;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Host;

namespace Isoplane.HotReload;

internal sealed class EditCompiler : IDisposable
{
    private readonly object service;
    private readonly object session;
    private readonly Type contract;
    private readonly object runningProjects;
    private readonly Type spanProvider;
    private Solution baseline;
    private readonly CompilerDebugger debugger;
    private Solution? pending;

    private EditCompiler(Solution baseline, object service, Type contract, object session,
        object runningProjects, Type spanProvider, CompilerDebugger debugger)
    {
        this.baseline = baseline;
        this.service = service;
        this.contract = contract;
        this.session = session;
        this.runningProjects = runningProjects;
        this.spanProvider = spanProvider;
        this.debugger = debugger;
    }

    internal static async Task<EditCompiler> CreateAsync(Workspace workspace, Solution baseline, ProjectId entry, string[] capabilities)
    {
        var features = Assembly.Load("Microsoft.CodeAnalysis.Features");
        var contract = features.GetType("Microsoft.CodeAnalysis.EditAndContinue.IEditAndContinueService", throwOnError: true)!;
        var workspaceContract = features.GetType("Microsoft.CodeAnalysis.EditAndContinue.IEditAndContinueWorkspaceService", throwOnError: true)!;
        var workspaceService = typeof(HostWorkspaceServices).GetMethod("GetService")!.MakeGenericMethod(workspaceContract).Invoke(workspace.Services, null)!;
        var service = Member(workspaceService, "Service");
        UseOwnedOutputs(service, features);
        var debuggerContract = features.GetType("Microsoft.CodeAnalysis.Contracts.EditAndContinue.IManagedHotReloadService", throwOnError: true)!;
        var debugger = (CompilerDebugger)DispatchProxy.Create(debuggerContract, typeof(CompilerDebugger));
        debugger.Initialize(features, capabilities);
        var pdbSource = features.GetType("Microsoft.CodeAnalysis.EditAndContinue.NullPdbMatchingSourceTextProvider", throwOnError: true)!.GetField("Instance")!.GetValue(null)!;
        var emit = contract.GetMethod("EmitSolutionUpdateAsync")!;
        var mapType = emit.GetParameters()[2].ParameterType;
        var runningProjects = mapType.GetMethod("Add")!.Invoke(mapType.GetField("Empty")!.GetValue(null),
            [entry, Activator.CreateInstance(mapType.GetGenericArguments()[1])])!;
        var spanProvider = emit.GetParameters()[3].ParameterType;
        var session = await StartSessionAsync(contract, service, baseline, debugger, pdbSource);
        return new(baseline, service, contract, session, runningProjects, spanProvider, debugger);
    }

    private static async Task<object> StartSessionAsync(Type contract, object service, Solution baseline,
        CompilerDebugger debugger, object pdbSource)
    {
        if (contract.GetMethod("StartDebuggingSession") is { } synchronous)
            return synchronous.Invoke(service, [baseline, debugger, pdbSource, false])
                ?? throw new IOException("The SDK did not start the Hot Reload session.");
        var asynchronous = contract.GetMethod("StartDebuggingSessionAsync")
            ?? throw new NotSupportedException("The SDK cannot start a Hot Reload debugging session.");
        var operation = asynchronous.Invoke(service,
            [baseline, debugger, pdbSource, ImmutableArray<DocumentId>.Empty, false, false, CancellationToken.None])!;
        var task = (Task)operation.GetType().GetMethod("AsTask")!.Invoke(operation, null)!;
        await task;
        return Member(task, "Result");
    }

    private static void UseOwnedOutputs(object service, Assembly features)
    {
        // Roslyn's default reader follows the original CodeView path instead of the owned symbol snapshot.
        var accessorMethod = service.GetType().GetMethod("GetTestAccessor", BindingFlags.Instance | BindingFlags.NonPublic)
            ?? throw new NotSupportedException("The SDK cannot configure the Hot Reload compiler's output provider.");
        var accessor = accessorMethod.Invoke(service, null)
            ?? throw new NotSupportedException("The SDK did not expose its Hot Reload output provider.");
        var configure = accessor.GetType().GetMethod("SetOutputProvider")
            ?? throw new NotSupportedException("The SDK cannot select owned Hot Reload compilation outputs.");
        var outputs = features.GetType("Microsoft.CodeAnalysis.Emit.CompilationOutputFiles", throwOnError: true)!;
        var constructor = outputs.GetConstructor([typeof(string), typeof(string)])
            ?? throw new NotSupportedException("The SDK cannot read explicit assembly and symbol paths.");
        var project = Expression.Parameter(typeof(Project), "project");
        var assembly = Expression.Property(Expression.Property(project, nameof(Project.CompilationOutputInfo)),
            nameof(CompilationOutputInfo.AssemblyPath));
        var symbols = Expression.Call(typeof(Path), nameof(Path.ChangeExtension), null, assembly, Expression.Constant(".pdb"));
        var symbolPath = Expression.Condition(Expression.Call(typeof(File), nameof(File.Exists), null, symbols),
            symbols, Expression.Constant(null, typeof(string)));
        var factory = Expression.Lambda(configure.GetParameters()[0].ParameterType,
            Expression.New(constructor, assembly, symbolPath), project).Compile();
        configure.Invoke(accessor, [factory]);
    }

    internal async Task<CompilerReply> EmitAsync(Solution updated, CompilerEdit edit)
    {
        if (pending is not null) throw new InvalidOperationException("Commit or discard the previous compiler update first.");
        debugger.SetState(edit);
        Call("BreakStateOrCapabilitiesChanged", session, true);
        var documents = baseline.Projects.SelectMany(project => project.DocumentIds).ToImmutableArray();
        var baseOperation = Call("GetBaseActiveStatementSpansAsync", session, baseline, documents, CancellationToken.None)!;
        var baseTask = (Task)baseOperation.GetType().GetMethod("AsTask")!.Invoke(baseOperation, null)!;
        await baseTask;
        var spans = await TrackedStatements.CreateAsync(spanProvider, baseline, updated, documents, Items(Member(baseTask, "Result")).ToArray());
        var operation = Call("EmitSolutionUpdateAsync", session, updated, runningProjects, spans, CancellationToken.None)!;
        var task = (Task)operation.GetType().GetMethod("AsTask")!.Invoke(operation, null)!;
        await task;
        var result = Member(task, "Result");
        var modules = Member(result, "ModuleUpdates");
        bool ready = Member(modules, "Status").ToString() switch
        {
            "Ready" => true,
            "None" or "Blocked" => false,
            var status => throw new NotSupportedException($"The SDK returned unsupported compiler status '{status}'.")
        };
        if (ready) pending = updated;
        var updates = Items(Member(modules, "Updates")).Select(module => new ModuleUpdate(
            (Guid)Member(module, "Module"), Bytes(module, "MetadataDelta"), Bytes(module, "ILDelta"), Bytes(module, "PdbDelta"))
        {
            UpdatedTypes = Items(Member(module, "UpdatedTypes")).Cast<int>().ToArray(),
            ActiveStatements = Items(Member(module, "ActiveStatements")).Select(ReadActiveStatementUpdate).ToArray(),
            SequencePoints = Items(Member(module, "SequencePoints")).Select(update => new SequencePointUpdate(
                (string)Member(update, "FileName"), Items(Member(update, "LineUpdates"))
                    .Select(line => new SourceLineUpdate((int)Member(line, "OldLine"), (int)Member(line, "NewLine"))).ToArray())).ToArray()
        }).ToArray();
        var diagnostics = Items(Member(result, "Diagnostics")).SelectMany(project => Items(Member(project, "Diagnostics")))
            .Cast<Diagnostic>().ToList();
        if (MemberOrNull(result, "SyntaxError") is Diagnostic syntax && !diagnostics.Contains(syntax)) diagnostics.Add(syntax);
        bool restart = Items(Member(result, "ProjectsToRestart")).Any() || Items(Member(result, "ProjectsToRebuild")).Any();
        return new(updates, diagnostics.Select(diagnostic => diagnostic.ToString()).ToArray(), restart) { Pending = ready };
    }

    internal Solution Commit()
    {
        var solution = pending ?? throw new InvalidOperationException("There is no pending compiler update to commit.");
        Call("CommitSolutionUpdate", session);
        pending = null;
        baseline = solution;
        return solution;
    }

    internal void Discard()
    {
        if (pending is null) throw new InvalidOperationException("There is no pending compiler update to discard.");
        Call("DiscardSolutionUpdate", session);
        pending = null;
    }

    public void Dispose()
    {
        if (pending is not null) Discard();
        Call("EndDebuggingSession", session);
    }

    private static ActiveStatementUpdate ReadActiveStatementUpdate(object value)
    {
        var method = Member(value, "Method");
        var span = Member(value, "NewSpan");
        return new((int)Member(method, "Token"), (int)Member(method, "Version"), (int)Member(value, "ILOffset"),
            new((int)Member(span, "StartLine"), (int)Member(span, "StartColumn"),
                (int)Member(span, "EndLine"), (int)Member(span, "EndColumn")));
    }

    private object? Call(string method, params object?[] arguments) =>
        (contract.GetMethod(method) ?? throw new NotSupportedException($"The SDK's Edit and Continue service has no '{method}' operation."))
        .Invoke(service, arguments);
    private static IEnumerable<object> Items(object value) => ((IEnumerable)value).Cast<object>();
    private static byte[] Bytes(object value, string member) => ((ImmutableArray<byte>)Member(value, member)).ToArray();
    private static object Member(object value, string member) => MemberOrNull(value, member)
        ?? throw new InvalidOperationException($"The SDK's Edit and Continue contract is missing '{member}'.");
    private static object? MemberOrNull(object value, string member)
    {
        if (value.GetType().GetProperty(member) is { } property) return property.GetValue(value);
        if (value.GetType().GetField(member) is { } field) return field.GetValue(value);
        throw new NotSupportedException($"The SDK's Edit and Continue contract has no '{member}' member.");
    }
}
