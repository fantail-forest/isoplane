using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using Isoplane.Build;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Mono.Debugger.Soft;
using Mono.Debugging.Client;
using Mono.Debugging.Evaluation;
using Mono.Debugging.Soft;

namespace Isoplane.Debugging;

internal sealed record AsyncFrame(AsyncDebugSymbols.Method Method, AsyncDebugSymbols.AwaitLocation Location,
    SoftEvaluationContext Context)
{
    internal ObjectValue[] Locals() => Context.Adapter.GetLocalVariables(Context)
        .Concat(Context.Adapter.GetParameters(Context)).Select(value => value.CreateObjectValue(false)).ToArray();

    internal ObjectValue Evaluate(string expression)
    {
        var value = Context.Evaluator.Evaluate(Context, expression);
        if (value is NamespaceValueReference)
            throw new EvaluatorException("The expression '{0}' does not resolve to a value in this async frame.", expression);
        return value.CreateObjectValue(false);
    }
}

internal static class AsyncFrames
{
    internal static IReadOnlyList<AsyncFrame> Read(SoftDebuggerSession session, GuestCode code,
        IReadOnlyList<Mono.Debugger.Soft.StackFrame> physical, int frameIndex)
    {
        var frame = physical[frameIndex];
        if (frame.Method.Name != "MoveNext" || frame.Method.IsStatic)
            return [];
        var task = frame.Domain.Corlib.GetType("System.Threading.Tasks.Task");
        var head = task.GetField("s_isoplaneAsyncState")
            ?? throw new ProtocolException("The guest runtime does not contain async debugger tracking.");
        if (task.GetValue(head) is not ObjectMirror tracking) return [];
        var nodes = Records(tracking, "Frames");
        var dependencies = Records(tracking, "Dependencies");
        var root = Root(nodes, physical, frameIndex);
        if (root is null) return [];
        var parents = Parents(nodes, dependencies);
        var result = new List<AsyncFrame>();
        var visited = new HashSet<ObjectMirror>();
        var pending = new Stack<Continuation>();
        pending.Push(new Continuation(Operation(root), null));
        while (pending.TryPop(out var current))
        {
            if (!visited.Add(current.Operation)) continue;
            if (current.Frame is { } saved && ReadCaller(session, code, frame, saved) is { } caller)
                result.Add(caller);
            if (parents.TryGetValue(current.Operation, out var callers))
                for (int index = callers.Count - 1; index >= 0; index--) pending.Push(callers[index]);
        }
        return result;
    }

    private sealed record Continuation(ObjectMirror Operation, ObjectMirror? Frame);

    private static Dictionary<ObjectMirror, List<Continuation>> Parents(List<ObjectMirror> frames, List<ObjectMirror> dependencies)
    {
        var result = new Dictionary<ObjectMirror, List<Continuation>>();
        foreach (var frame in frames) Add(frame, frame);
        foreach (var dependency in dependencies) Add(dependency, null);
        return result;

        void Add(ObjectMirror node, ObjectMirror? frame)
        {
            if (Field(node, "Awaited") is not ObjectMirror awaited) return;
            if (!result.TryGetValue(awaited, out var parents)) result.Add(awaited, parents = []);
            parents.Add(new Continuation(Operation(node), frame));
        }
    }

    private static List<ObjectMirror> Records(ObjectMirror tracking, string field)
    {
        var result = new List<ObjectMirror>();
        var seen = new HashSet<ObjectMirror>();
        for (var node = Field(tracking, field) as ObjectMirror; node is not null && seen.Add(node); node = Field(node, "Next") as ObjectMirror)
            result.Add(node);
        return result;
    }

    private static ObjectMirror Operation(ObjectMirror node) => Field(node, "Operation") as ObjectMirror
        ?? throw new ProtocolException("The guest async tracking record does not contain its operation.");

    private static AsyncFrame? ReadCaller(SoftDebuggerSession session, GuestCode code,
        Mono.Debugger.Soft.StackFrame frame, ObjectMirror node)
    {
        var owner = Field(node, "Operation") as ObjectMirror;
        var storageField = owner is null ? null : StateMachineField(owner.Type);
        if (owner is null || storageField is null) return null;
        var state = owner.GetValue(storageField);
        var stateType = StateType(state);
        if (stateType is null) return null;
        var method = stateType.GetMethod("MoveNext");
        var assembly = method.DeclaringType.Assembly;
        var pdb = code.Contains(method) && assembly.HasDebugInfoLoaded() ? assembly.GetPdbBlob() : null;
        if (pdb is null) return null;
        using var pe = new PEReader(new MemoryStream(assembly.GetMetadataBlob()));
        using var symbols = MetadataReaderProvider.FromPortablePdbStream(new MemoryStream(pdb));
        var info = AsyncDebugSymbols.Read(pe.GetMetadataReader(), symbols.GetMetadataReader(), (MethodDefinitionHandle)MetadataTokens.Handle(method.MetadataToken));
        var stateField = stateType.GetField("<>1__state");
        var stateValue = stateField is null ? null : state switch
        {
            ObjectMirror value => value.GetValue(stateField),
            StructMirror value => value[stateField.Name],
            _ => null
        };
        if (info is null || stateValue is not PrimitiveValue { Value: int index }
            || index < 0 || index >= info.Awaits.Length) return null;
        var context = new SoftEvaluationContext(session, frame, EvaluationOptions.DefaultOptions);
        var storage = new FieldValueReference(context, storageField, owner, owner.Type);
        context.Adapter = new AsyncVariables(storage, stateType, assembly.GetType(info.DeclaringType)
            ?? throw new ProtocolException("The async caller's declaring type is unavailable.")) { Session = session };
        return new AsyncFrame(info, info.Awaits[index], context);
    }

    private static ObjectMirror? Root(List<ObjectMirror> nodes, IReadOnlyList<Mono.Debugger.Soft.StackFrame> physical, int frameIndex)
    {
        var frame = physical[frameIndex];
        if (frame.GetThis() is ObjectMirror current)
            return nodes.SingleOrDefault(node => ReferenceEquals(Field(node, "StateMachine"), current));
        foreach (var caller in physical.Skip(frameIndex + 1))
        {
            if (caller.Method.Name != "MoveNext" || caller.Method.IsStatic || caller.GetThis() is not ObjectMirror owner) continue;
            var node = nodes.SingleOrDefault(node => ReferenceEquals(Field(node, "Operation"), owner)
                && StateType(Field(node, "StateMachine")) == frame.Method.DeclaringType);
            if (node is not null) return node;
        }
        return null;
    }

    private static TypeMirror? StateType(Value value) => value switch
    {
        ObjectMirror instance => instance.Type,
        StructMirror instance => instance.Type,
        _ => null
    };

    private static FieldInfoMirror? StateMachineField(TypeMirror type)
    {
        for (TypeMirror? current = type; current is not null; current = current.BaseType)
            if (current.GetField("StateMachine") is { } field) return field;
        return null;
    }

    private static Value Field(ObjectMirror instance, string name) => instance.GetValue(instance.Type.GetField(name)
        ?? throw new ProtocolException("The guest async tracking record has no " + name + " field."));
}
