using Mono.Cecil;
using Mono.Cecil.Cil;

namespace Isoplane.Debugging;

internal static class StatementFlow
{
    internal static HashSet<int> EmptyStackOffsets(MethodDefinition method)
    {
        if (!method.HasBody || method.IsConstructor || method.IsSynchronized || method.Body.HasExceptionHandlers
            || method.Name == "MoveNext" || method.CustomAttributes.Any(attribute => attribute.AttributeType.FullName is
                "System.Runtime.CompilerServices.AsyncStateMachineAttribute" or "System.Runtime.CompilerServices.IteratorStateMachineAttribute"))
            return [];
        var instructions = method.Body.Instructions;
        if (instructions.Count == 0 || instructions.Any(instruction => instruction.OpCode.Code is Code.Localloc or Code.Jmp or Code.Calli)) return [];
        var depths = new Dictionary<Instruction, int>();
        var pending = new Queue<(Instruction Instruction, int Depth)>();
        pending.Enqueue((instructions[0], 0));
        while (pending.TryDequeue(out var item))
        {
            var (instruction, depth) = item;
            if (depths.TryGetValue(instruction, out int previous))
            {
                if (previous != depth) return [];
                continue;
            }
            depths.Add(instruction, depth);
            var (pop, push) = StackEffect(method, instruction);
            if (pop < 0 || push < 0 || depth < pop) return [];
            int nextDepth = depth - pop + push;
            switch (instruction.OpCode.FlowControl)
            {
                case FlowControl.Branch:
                    if (instruction.Operand is not Instruction target) return [];
                    pending.Enqueue((target, nextDepth));
                    break;
                case FlowControl.Cond_Branch:
                    if (instruction.Operand is Instruction branch) pending.Enqueue((branch, nextDepth));
                    else if (instruction.Operand is Instruction[] targets)
                        foreach (var choice in targets) pending.Enqueue((choice, nextDepth));
                    else return [];
                    goto case FlowControl.Next;
                case FlowControl.Next:
                case FlowControl.Call:
                case FlowControl.Break:
                case FlowControl.Meta:
                    if (instruction.Next is null) return [];
                    pending.Enqueue((instruction.Next, nextDepth));
                    break;
                case FlowControl.Return:
                case FlowControl.Throw:
                    if (nextDepth != 0) return [];
                    break;
                default:
                    return [];
            }
        }
        return depths.Where(item => item.Value == 0 && item.Key.Previous?.OpCode.OpCodeType != OpCodeType.Prefix)
            .Select(item => item.Key.Offset).ToHashSet();
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
