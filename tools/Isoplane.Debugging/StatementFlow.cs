using Mono.Cecil;
using Mono.Cecil.Cil;
using Mono.Debugger.Soft;

namespace Isoplane.Debugging;

internal static class StatementFlow
{
    internal static bool CanMove(MethodDefinition method) => method.HasBody && !method.IsConstructor && !method.IsSynchronized
        && (method.Name != "MoveNext" || IsAsyncBody(method))
        && !method.CustomAttributes.Any(attribute => attribute.AttributeType.FullName is
            "System.Runtime.CompilerServices.AsyncStateMachineAttribute" or "System.Runtime.CompilerServices.IteratorStateMachineAttribute");

    internal static HashSet<int> EmptyStackOffsets(MethodDefinition method) =>
        CanMove(method) ? EmptyStackOffsets(Capture(method)) : [];

    internal sealed record Node(int Offset, OpCode OpCode, int Pop, int Push, int[] Branches);
    internal sealed record Handler(int Start, int End, int? Filter, bool ExceptionOnStack);
    internal sealed record Body(Node[] Instructions, Handler[] Handlers);

    internal static Body Capture(MethodBodyMirror body) => new(
        body.Instructions.Select(instruction =>
        {
            var (pop, push) = StackEffect(body.Method, instruction);
            return new Node(instruction.Offset, instruction.OpCode, pop, push, instruction.Operand switch
            {
                ILInstruction target => [target.Offset],
                ILInstruction[] targets => targets.Select(target => target.Offset).ToArray(),
                _ => []
            });
        }).ToArray(),
        body.ExceptionHandlers.Select(handler => new Handler(handler.HandlerOffset,
            handler.HandlerOffset + handler.HandlerLength,
            handler.HandlerType == ILExceptionHandlerType.Filter ? handler.FilterOffset : null,
            handler.HandlerType is ILExceptionHandlerType.Catch or ILExceptionHandlerType.Filter)).ToArray());

    private static Body Capture(MethodDefinition method) => new(
        method.Body.Instructions.Select(instruction =>
        {
            var (pop, push) = StackEffect(method, instruction);
            return new Node(instruction.Offset, instruction.OpCode, pop, push, instruction.Operand switch
            {
                Instruction target => [target.Offset],
                Instruction[] targets => targets.Select(target => target.Offset).ToArray(),
                _ => []
            });
        }).ToArray(),
        method.Body.ExceptionHandlers.Select(handler => new Handler(handler.HandlerStart.Offset,
            handler.HandlerEnd?.Offset ?? method.Body.CodeSize, handler.FilterStart?.Offset,
            handler.HandlerType is ExceptionHandlerType.Catch or ExceptionHandlerType.Filter)).ToArray());

    private static HashSet<int> EmptyStackOffsets(Body body)
    {
        var instructions = body.Instructions;
        if (instructions.Length == 0 || instructions.Any(instruction => instruction.OpCode.Code is Code.Localloc or Code.Jmp or Code.Calli)) return [];
        var depths = new Dictionary<int, int>();
        var pending = new Queue<(int Offset, int Depth)>();
        pending.Enqueue((instructions[0].Offset, 0));
        var indices = instructions.Select((instruction, index) => (instruction.Offset, index)).ToDictionary(item => item.Offset, item => item.index);
        foreach (var handler in body.Handlers)
        {
            pending.Enqueue((handler.Start, handler.ExceptionOnStack ? 1 : 0));
            if (handler.Filter is { } filter) pending.Enqueue((filter, 1));
        }
        while (pending.TryDequeue(out var item))
        {
            var (offset, depth) = item;
            if (!indices.TryGetValue(offset, out int index)) return [];
            var instruction = instructions[index];
            if (depths.TryGetValue(offset, out int previous))
            {
                if (previous != depth) return [];
                continue;
            }
            depths.Add(offset, depth);
            if (instruction.OpCode.Code is Code.Leave or Code.Leave_S)
            {
                if (instruction.Branches.Length != 1) return [];
                pending.Enqueue((instruction.Branches[0], 0));
                continue;
            }
            var (pop, push) = (instruction.Pop, instruction.Push);
            if (pop < 0 || push < 0 || depth < pop) return [];
            int nextDepth = depth - pop + push;
            switch (instruction.OpCode.FlowControl)
            {
                case FlowControl.Branch:
                    if (instruction.Branches.Length != 1) return [];
                    pending.Enqueue((instruction.Branches[0], nextDepth));
                    break;
                case FlowControl.Cond_Branch:
                    foreach (int choice in instruction.Branches) pending.Enqueue((choice, nextDepth));
                    goto case FlowControl.Next;
                case FlowControl.Next:
                case FlowControl.Call:
                case FlowControl.Break:
                case FlowControl.Meta:
                    if (index + 1 == instructions.Length) return [];
                    pending.Enqueue((instructions[index + 1].Offset, nextDepth));
                    break;
                case FlowControl.Return:
                case FlowControl.Throw:
                    if (nextDepth != 0) return [];
                    break;
                default:
                    return [];
            }
        }
        return depths.Where(item => item.Value == 0 && (indices[item.Key] == 0
                || instructions[indices[item.Key] - 1].OpCode.OpCodeType != OpCodeType.Prefix))
            .Select(item => item.Key).ToHashSet();
    }

    internal static bool IsAsyncBody(MethodDefinition method) => method.Name == "MoveNext"
        && method.DeclaringType.DeclaringType is { } owner
        && owner.Methods.Any(candidate => candidate.CustomAttributes.Any(attribute =>
            attribute.AttributeType.FullName == "System.Runtime.CompilerServices.AsyncStateMachineAttribute"
            && attribute.ConstructorArguments.Count == 1
            && attribute.ConstructorArguments[0].Value is TypeReference state
            && state.FullName == method.DeclaringType.FullName));

    internal static HashSet<int> ReachableTargets(MethodDefinition method, int currentOffset)
    {
        if (!CanMove(method)) return [];
        var body = Capture(method);
        return ReachableTargets(body, body, currentOffset);
    }

    internal static HashSet<int> ReachableTargets(Body active, Body updated, int currentOffset)
    {
        if (!EmptyStackOffsets(active).Contains(currentOffset)) return [];
        var offsets = EmptyStackOffsets(updated);
        if (active.Handlers.Length != updated.Handlers.Length) return [];
        for (int index = 0; index < active.Handlers.Length; index++)
        {
            var original = active.Handlers[index];
            var destination = updated.Handlers[index];
            if (original.Filter is { } filter && InRegion(currentOffset, filter, original.Start)) return [];
            bool insideHandler = InRegion(currentOffset, original.Start, original.End);
            offsets.RemoveWhere(offset => InRegion(offset, destination.Start, destination.End) != insideHandler
                || destination.Filter is { } start && InRegion(offset, start, destination.Start));
        }
        return offsets;
    }

    private static bool InRegion(int offset, int start, int end) => offset >= start && offset < end;

    private static (int Pop, int Push) StackEffect(MethodMirror method, ILInstruction instruction)
    {
        if (instruction.OpCode.Code == Code.Ret) return (method.ReturnType.FullName == "System.Void" ? 0 : 1, 0);
        if (instruction.OpCode.Code is Code.Call or Code.Callvirt or Code.Newobj)
        {
            if (instruction.Operand is not MethodMirror called || called.HasVariableArguments) return (-1, -1);
            bool constructor = instruction.OpCode.Code == Code.Newobj;
            return (called.GetParameters().Length + (!constructor && !called.IsStatic ? 1 : 0),
                constructor || called.ReturnType.FullName != "System.Void" ? 1 : 0);
        }
        return (Count(instruction.OpCode.StackBehaviourPop), Count(instruction.OpCode.StackBehaviourPush));
    }

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
        return (Count(instruction.OpCode.StackBehaviourPop), Count(instruction.OpCode.StackBehaviourPush));
    }

    private static int Count(StackBehaviour behavior) => behavior switch
    {
        StackBehaviour.Pop0 or StackBehaviour.Push0 => 0,
        StackBehaviour.Pop1 or StackBehaviour.Popi or StackBehaviour.Popref or StackBehaviour.Push1
            or StackBehaviour.Pushi or StackBehaviour.Pushi8 or StackBehaviour.Pushr4 or StackBehaviour.Pushr8 or StackBehaviour.Pushref => 1,
        StackBehaviour.Pop1_pop1 or StackBehaviour.Popi_pop1 or StackBehaviour.Popi_popi or StackBehaviour.Popi_popi8
            or StackBehaviour.Popi_popr4 or StackBehaviour.Popi_popr8 or StackBehaviour.Popref_pop1
            or StackBehaviour.Popref_popi or StackBehaviour.Push1_push1 => 2,
        StackBehaviour.Popi_popi_popi or StackBehaviour.Popref_popi_popi or StackBehaviour.Popref_popi_popi8
            or StackBehaviour.Popref_popi_popr4 or StackBehaviour.Popref_popi_popr8 or StackBehaviour.Popref_popi_popref => 3,
        _ => -1
    };
}
