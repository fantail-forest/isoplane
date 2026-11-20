using Mono.Cecil;
using Mono.Cecil.Cil;
using Isoplane.Debugging;

/// <summary>
/// Builds the debugger's statement-flow model from IL the tests emit with Cecil. The debugger builds
/// the same model from a live method mirror; the rules the tests check live in the shared model.
/// </summary>
internal static class EmittedStatements
{
    internal static bool CanMove(MethodDefinition method) => method.HasBody
        && !method.CustomAttributes.Any(attribute => attribute.AttributeType.FullName is
            "System.Runtime.CompilerServices.AsyncStateMachineAttribute" or "System.Runtime.CompilerServices.IteratorStateMachineAttribute");

    internal static HashSet<int> EmptyStackOffsets(MethodDefinition method) =>
        CanMove(method) ? StatementFlow.EmptyStackOffsets(Capture(method)) : [];

    internal static HashSet<int> ReachableTargets(MethodDefinition method, int currentOffset)
    {
        if (!CanMove(method)) return [];
        var body = Capture(method);
        return StatementFlow.ReachableTargets(body, body, currentOffset);
    }

    private static StatementFlow.Body Capture(MethodDefinition method) => new(
        method.Body.Instructions.Select(instruction =>
        {
            var (pop, push) = StackEffect(method, instruction);
            return new StatementFlow.Node(instruction.Offset, instruction.OpCode, pop, push, instruction.Operand switch
            {
                Instruction target => [target.Offset],
                Instruction[] targets => targets.Select(target => target.Offset).ToArray(),
                _ => []
            });
        }).ToArray(),
        method.Body.ExceptionHandlers.Select(handler => new StatementFlow.Handler(handler.HandlerStart.Offset,
            handler.HandlerEnd?.Offset ?? method.Body.CodeSize, handler.FilterStart?.Offset,
            handler.HandlerType is ExceptionHandlerType.Catch or ExceptionHandlerType.Filter)).ToArray());

    private static (int Pop, int Push) StackEffect(MethodDefinition method, Instruction instruction)
    {
        if (instruction.OpCode.Code == Code.Ret) return (method.ReturnType.MetadataType == MetadataType.Void ? 0 : 1, 0);
        if (instruction.OpCode.Code is Code.Call or Code.Callvirt or Code.Newobj)
        {
            if (instruction.Operand is not MethodReference called || called.CallingConvention == MethodCallingConvention.VarArg) return (-1, -1);
            bool constructor = instruction.OpCode.Code == Code.Newobj;
            return (called.Parameters.Count + (!constructor && called.HasThis ? 1 : 0),
                constructor || called.ReturnType.MetadataType != MetadataType.Void ? 1 : 0);
        }
        return (StatementFlow.Count(instruction.OpCode.StackBehaviourPop), StatementFlow.Count(instruction.OpCode.StackBehaviourPush));
    }
}
