using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class Polling
{
    internal static MethodDefinition Adapt(ModuleDefinition module)
    {
        var loop = module.Types.Single(type => type.FullName == "System.Threading.WasiEventLoop");
        var check = loop.Methods.Single(method => method.Name == "CheckPollables");
        var counter = check.Body.Instructions.Single(instruction => instruction.OpCode == OpCodes.Call
            && instruction.Operand is MethodReference method && method.DeclaringType.FullName == "System.Threading.ThreadPool"
            && method.Name == "get_PendingWorkItemCount" && method.ReturnType.FullName == "System.Int64");
        var reschedule = check.Body.Instructions.Single(instruction => instruction.OpCode == OpCodes.Call
            && instruction.Operand is MethodReference method && method.DeclaringType == loop && method.Name == "ScheduleCheck");
        // Force the existing ready-pollable branch, so the WASI poll remains conformant and cannot block.
        counter.OpCode = OpCodes.Ldc_I8;
        counter.Operand = 1L;
        // The host schedules checks when resources become ready; pending resources must not cause a busy loop.
        reschedule.OpCode = OpCodes.Nop;
        reschedule.Operand = null;
        // A callback can contribute multiple pollables; preserve registrations independently
        // of the per-pollable callback lookup so one registration is retained exactly once.
        var registrations = loop.Fields.Single(field => field.Name == "s_hooks");
        var retain = check.Body.Instructions.Single(instruction => instruction.OpCode == OpCodes.Callvirt
            && instruction.Operand is MethodReference method && method.Name == "Add"
            && instruction.Previous.Previous.OpCode == OpCodes.Ldsfld
            && instruction.Previous.Previous.Operand == registrations);
        var retentionIndex = check.Body.Instructions.IndexOf(retain);
        var item = check.Body.Instructions.Take(retentionIndex).Last(instruction =>
            instruction.Operand is MethodReference method && method.Name == "get_Item"
            && method.DeclaringType.FullName == registrations.FieldType.FullName);
        var count = check.Body.Instructions.Skip(retentionIndex + 1).Single(instruction =>
            instruction.Operand is MethodReference method && method.Name == "get_Count"
            && method.DeclaringType.FullName == registrations.FieldType.FullName);
        var itemSource = item.Previous.Previous;
        var countSource = count.Previous;
        if (LoadVariable(check, itemSource) != LoadVariable(check, countSource))
            throw new InvalidOperationException("The registration retention loop changed shape.");

        var snapshot = new VariableDefinition(registrations.FieldType);
        check.Body.Variables.Add(snapshot);
        var list = module.Types.Single(type => type.FullName == "System.Collections.Generic.List`1");
        var definition = list.Methods.Single(method => method.IsConstructor && method.Parameters.Count == 1
            && method.Parameters[0].ParameterType is GenericInstanceType parameter
            && parameter.ElementType.FullName == "System.Collections.Generic.IEnumerable`1");
        var constructor = new MethodReference(".ctor", module.TypeSystem.Void, registrations.FieldType) { HasThis = true };
        constructor.Parameters.Add(new ParameterDefinition(definition.Parameters[0].ParameterType));
        var il = check.Body.GetILProcessor();
        var first = check.Body.Instructions.First();
        il.InsertBefore(first, Instruction.Create(OpCodes.Ldsfld, registrations));
        il.InsertBefore(first, Instruction.Create(OpCodes.Newobj, constructor));
        il.InsertBefore(first, Instruction.Create(OpCodes.Stloc, snapshot));
        foreach (var load in new[] { itemSource, countSource })
        {
            load.OpCode = OpCodes.Ldloc;
            load.Operand = snapshot;
        }
        // Re-encoding branch distances keeps the edited method valid across preview builds.
        var opcodes = typeof(OpCodes).GetFields().Where(field => field.FieldType == typeof(OpCode))
            .Select(field => (OpCode)field.GetValue(null)!).ToDictionary(opcode => opcode.Name);
        foreach (var instruction in check.Body.Instructions)
            if (instruction.OpCode.OperandType == OperandType.ShortInlineBrTarget)
                instruction.OpCode = opcodes[instruction.OpCode.Name[..^2]];
        return loop.Methods.Single(method => method.Name == "ScheduleCheck");
    }

    static VariableDefinition LoadVariable(MethodDefinition method, Instruction instruction) => instruction.OpCode.Code switch
    {
        Code.Ldloc_0 => method.Body.Variables[0],
        Code.Ldloc_1 => method.Body.Variables[1],
        Code.Ldloc_2 => method.Body.Variables[2],
        Code.Ldloc_3 => method.Body.Variables[3],
        Code.Ldloc or Code.Ldloc_S => (VariableDefinition)instruction.Operand,
        _ => throw new InvalidOperationException("Expected a local load in the registration retention loop.")
    };
}
