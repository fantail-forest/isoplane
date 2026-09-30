using Mono.Cecil;
using Mono.Cecil.Cil;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class StatementFlowTests
{
    [Fact]
    public void only_empty_stack_instruction_boundaries_can_be_selected()
    {
        using var module = ModuleDefinition.CreateModule("Statements", ModuleKind.Dll);
        var method = Method(module);
        var il = method.Body.GetILProcessor();
        il.Emit(OpCodes.Ldc_I4_1);
        il.Emit(OpCodes.Ldc_I4_2);
        il.Emit(OpCodes.Add);
        il.Emit(OpCodes.Pop);
        il.Emit(OpCodes.Ret);
        Assert.Equal([0, 4], Offsets(module));
    }

    [Fact]
    public void branch_merges_must_agree_on_the_stack_depth()
    {
        using var module = ModuleDefinition.CreateModule("Statements", ModuleKind.Dll);
        var method = Method(module);
        var il = method.Body.GetILProcessor();
        var end = Instruction.Create(OpCodes.Ret);
        il.Emit(OpCodes.Ldc_I4_0);
        il.Emit(OpCodes.Brtrue_S, end);
        il.Emit(OpCodes.Ldc_I4_1);
        il.Append(end);
        Assert.Empty(Offsets(module));
    }

    [Fact]
    public void calls_consume_arguments_and_produce_their_return_value()
    {
        using var module = ModuleDefinition.CreateModule("Statements", ModuleKind.Dll);
        var method = Method(module);
        var il = method.Body.GetILProcessor();
        il.Emit(OpCodes.Ldc_I4_1);
        il.Emit(OpCodes.Call, module.ImportReference(typeof(Math).GetMethod(nameof(Math.Abs), [typeof(int)])!));
        il.Emit(OpCodes.Pop);
        il.Emit(OpCodes.Ret);
        Assert.Equal([0, 7], Offsets(module));
    }

    [Fact]
    public void async_state_machines_cannot_move_execution()
    {
        using var module = ModuleDefinition.CreateModule("Statements", ModuleKind.Dll);
        var method = Method(module);
        method.Name = "MoveNext";
        method.Body.GetILProcessor().Emit(OpCodes.Ret);
        Assert.Empty(Offsets(module));
    }

    [Fact]
    public void exception_regions_cannot_be_bypassed()
    {
        using var module = ModuleDefinition.CreateModule("Statements", ModuleKind.Dll);
        var method = Method(module);
        var il = method.Body.GetILProcessor();
        var start = Instruction.Create(OpCodes.Nop);
        var handler = Instruction.Create(OpCodes.Endfinally);
        var end = Instruction.Create(OpCodes.Ret);
        il.Append(start);
        il.Emit(OpCodes.Leave_S, end);
        il.Append(handler);
        il.Append(end);
        method.Body.ExceptionHandlers.Add(new ExceptionHandler(ExceptionHandlerType.Finally)
            { TryStart = start, TryEnd = handler, HandlerStart = handler, HandlerEnd = end });
        Assert.Empty(Offsets(module));
    }

    private static MethodDefinition Method(ModuleDefinition module)
    {
        var type = new TypeDefinition("Customer", "Application", TypeAttributes.Public, module.TypeSystem.Object);
        module.Types.Add(type);
        var method = new MethodDefinition("Compute", MethodAttributes.Public | MethodAttributes.Static, module.TypeSystem.Void);
        type.Methods.Add(method);
        return method;
    }

    private static int[] Offsets(ModuleDefinition module)
    {
        using var bytes = new MemoryStream();
        module.Write(bytes);
        bytes.Position = 0;
        using var loaded = ModuleDefinition.ReadModule(bytes);
        return StatementFlow.EmptyStackOffsets(loaded.Types.Single(type => type.Name == "Application").Methods.Single()).Order().ToArray();
    }
}
