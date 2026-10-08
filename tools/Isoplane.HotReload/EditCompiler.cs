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
    private readonly Delegate spans;
    private readonly CompilerDebugger debugger;
    private Solution? pending;

    internal EditCompiler(Workspace workspace, Solution baseline, ProjectId entry, string[] capabilities)
    {
        var features = Assembly.Load("Microsoft.CodeAnalysis.Features");
        contract = features.GetType("Microsoft.CodeAnalysis.EditAndContinue.IEditAndContinueService", throwOnError: true)!;
        var workspaceContract = features.GetType("Microsoft.CodeAnalysis.EditAndContinue.IEditAndContinueWorkspaceService", throwOnError: true)!;
        var workspaceService = typeof(HostWorkspaceServices).GetMethod("GetService")!.MakeGenericMethod(workspaceContract).Invoke(workspace.Services, null)!;
        service = Member(workspaceService, "Service");
        var debuggerContract = features.GetType("Microsoft.CodeAnalysis.Contracts.EditAndContinue.IManagedHotReloadService", throwOnError: true)!;
        debugger = (CompilerDebugger)DispatchProxy.Create(debuggerContract, typeof(CompilerDebugger));
        debugger.Initialize(features, capabilities);
        var pdbSource = features.GetType("Microsoft.CodeAnalysis.EditAndContinue.NullPdbMatchingSourceTextProvider", throwOnError: true)!.GetField("Instance")!.GetValue(null)!;
        session = Call("StartDebuggingSession", baseline, debugger, pdbSource, false)!;
        var emit = contract.GetMethod("EmitSolutionUpdateAsync")!;
        var mapType = emit.GetParameters()[2].ParameterType;
        runningProjects = mapType.GetMethod("Add")!.Invoke(mapType.GetField("Empty")!.GetValue(null),
            [entry, Activator.CreateInstance(mapType.GetGenericArguments()[1])])!;
        var provider = emit.GetParameters()[3].ParameterType;
        var invoke = provider.GetMethod("Invoke")!;
        var empty = invoke.ReturnType.GetGenericArguments()[0].GetField("Empty")!.GetValue(null)!;
        var completed = Activator.CreateInstance(invoke.ReturnType, empty)!;
        spans = Expression.Lambda(provider, Expression.Constant(completed, invoke.ReturnType),
            invoke.GetParameters().Select(parameter => Expression.Parameter(parameter.ParameterType, parameter.Name))).Compile();
    }

    internal async Task<CompilerReply> EmitAsync(Solution updated, CompilerEdit edit)
    {
        if (pending is not null) throw new InvalidOperationException("Commit or discard the previous compiler update first.");
        debugger.SetState(edit);
        Call("BreakStateOrCapabilitiesChanged", session, true);
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
            (Guid)Member(module, "Module"), Bytes(module, "MetadataDelta"), Bytes(module, "ILDelta"), Bytes(module, "PdbDelta"))).ToArray();
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
