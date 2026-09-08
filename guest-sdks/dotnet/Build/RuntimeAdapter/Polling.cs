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
        // Re-encoding branch distances keeps the edited method valid across preview builds.
        var opcodes = typeof(OpCodes).GetFields().Where(field => field.FieldType == typeof(OpCode))
            .Select(field => (OpCode)field.GetValue(null)!).ToDictionary(opcode => opcode.Name);
        foreach (var instruction in check.Body.Instructions)
            if (instruction.OpCode.OperandType == OperandType.ShortInlineBrTarget)
                instruction.OpCode = opcodes[instruction.OpCode.Name[..^2]];
        return loop.Methods.Single(method => method.Name == "ScheduleCheck");
    }

}
