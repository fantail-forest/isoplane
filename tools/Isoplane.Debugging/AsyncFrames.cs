using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using Isoplane.Build;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Mono.Debugger.Soft;
using Mono.Debugging.Client;
using Mono.Debugging.Soft;

namespace Isoplane.Debugging;

internal sealed record AsyncFrame(ObjectMirror State, AsyncDebugSymbols.Method Method, AsyncDebugSymbols.AwaitLocation Location,
    SoftEvaluationContext Context)
{
    internal ObjectValue[] Locals() => State.Type.GetFields().Where(field => !field.IsStatic)
        .Select(field => (Field: field, Name: LocalName(field.Name))).Where(field => field.Name is not null)
        .Select(field => new FieldValueReference(Context, field.Field, State, State.Type, field.Name!, ObjectValueFlags.Variable)
            .CreateObjectValue(false)).ToArray();

    private static string? LocalName(string name)
    {
        if (name == "<>4__this") return "this";
        if (!name.StartsWith('<')) return name;
        int end = name.IndexOf(">5__", StringComparison.Ordinal);
        return end > 1 ? name[1..end] : null;
    }
}

internal static class AsyncFrames
{
    internal static IReadOnlyList<AsyncFrame> Read(SoftDebuggerSession session, GuestCode code)
    {
        var threads = session.VirtualMachine.GetThreads();
        if (threads.Count != 1) throw new ProtocolException("Async stack reconstruction requires one guest thread.");
        var physical = threads[0].GetFrames();
        if (physical.Length == 0 || physical[0].Method.Name != "MoveNext" || physical[0].Method.IsStatic
            || physical[0].GetThis() is not ObjectMirror current)
            return [];
        var task = physical[0].Domain.Corlib.GetType("System.Threading.Tasks.Task");
        var head = task.GetField("s_isoplaneAsyncState")
            ?? throw new ProtocolException("The guest runtime does not contain async debugger tracking.");
        if (task.GetValue(head) is not ObjectMirror tracking) return [];
        var nodes = new List<ObjectMirror>();
        var seen = new HashSet<ObjectMirror>();
        for (var node = Field(tracking, "Frames") as ObjectMirror; node is not null && seen.Add(node); node = Field(node, "Next") as ObjectMirror)
            nodes.Add(node);
        var root = nodes.SingleOrDefault(node => ReferenceEquals(Field(node, "StateMachine"), current));
        if (root is null) return [];
        var result = new List<AsyncFrame>();
        var context = new SoftEvaluationContext(session, physical[0], EvaluationOptions.DefaultOptions);
        var visited = new HashSet<ObjectMirror> { root };
        Append(root);
        return result;

        void Append(ObjectMirror child)
        {
            var operation = Field(child, "Operation");
            foreach (var parent in nodes.Where(node => ReferenceEquals(Field(node, "Awaited"), operation)))
            {
                if (!visited.Add(parent)) continue;
                if (Field(parent, "StateMachine") is not ObjectMirror state) continue;
                if (state.Type.IsValueType)
                {
                    Append(parent);
                    continue;
                }
                var method = state.Type.GetMethod("MoveNext");
                var assembly = method.DeclaringType.Assembly;
                var pdb = code.Contains(method) && assembly.HasDebugInfoLoaded() ? assembly.GetPdbBlob() : null;
                if (pdb is not null)
                {
                    using var pe = new PEReader(new MemoryStream(assembly.GetMetadataBlob()));
                    using var symbols = MetadataReaderProvider.FromPortablePdbStream(new MemoryStream(pdb));
                    var info = AsyncDebugSymbols.Read(pe.GetMetadataReader(), symbols.GetMetadataReader(), (MethodDefinitionHandle)MetadataTokens.Handle(method.MetadataToken));
                    var stateField = state.Type.GetField("<>1__state");
                    if (info is not null && stateField is not null && state.GetValue(stateField) is PrimitiveValue { Value: int index }
                        && index >= 0 && index < info.Awaits.Length)
                        result.Add(new AsyncFrame(state, info, info.Awaits[index], context));
                }
                Append(parent);
            }
        }
    }

    private static Value Field(ObjectMirror instance, string name) => instance.GetValue(instance.Type.GetField(name)
        ?? throw new ProtocolException("The guest async tracking record has no " + name + " field."));
}
