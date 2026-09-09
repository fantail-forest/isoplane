using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class ManagedPbkdf2
{
    internal static void Adapt(ModuleDefinition module)
    {
        var derive = module.Types.Single(type => type.FullName == "System.Security.Cryptography.Rfc2898DeriveBytes");
        var constructor = RuntimeMethods.Require(derive, ".ctor", false, "System.Void",
            "System.ReadOnlySpan`1<System.Byte>", "System.ReadOnlySpan`1<System.Byte>",
            "System.Int32", "System.Security.Cryptography.HashAlgorithmName");
        var getBytes = RuntimeMethods.Require(derive, "GetBytes", false, "System.Void", "System.Span`1<System.Byte>");
        var parent = module.Types.Single(type => type.FullName == "System.Security.Cryptography.DeriveBytes");
        var dispose = RuntimeMethods.Require(parent, "Dispose", false, "System.Void");
        var implementation = module.Types.Single(type => type.FullName == "System.Security.Cryptography.Pbkdf2Implementation");
        var fill = RuntimeMethods.Require(implementation, "Fill", true, "System.Void",
            "System.ReadOnlySpan`1<System.Byte>", "System.ReadOnlySpan`1<System.Byte>",
            "System.Int32", "System.Security.Cryptography.HashAlgorithmName", "System.Span`1<System.Byte>");
        Rewrite(fill, constructor, getBytes, dispose);
    }

    private static void Rewrite(MethodDefinition fill, MethodDefinition constructor,
        MethodDefinition getBytes, MethodDefinition dispose)
    {
        fill.Body = new MethodBody(fill) { InitLocals = true };
        var instance = new VariableDefinition(constructor.DeclaringType);
        fill.Body.Variables.Add(instance);
        var il = fill.Body.GetILProcessor();
        foreach (var argument in fill.Parameters.Take(constructor.Parameters.Count))
            il.Emit(OpCodes.Ldarg, argument);
        il.Emit(OpCodes.Newobj, constructor);
        il.Emit(OpCodes.Stloc, instance);
        var begin = Instruction.Create(OpCodes.Ldloc, instance);
        var cleanup = Instruction.Create(OpCodes.Ldloc, instance);
        var end = Instruction.Create(OpCodes.Ret);
        il.Append(begin);
        il.Emit(OpCodes.Ldarg, fill.Parameters[^1]);
        il.Emit(OpCodes.Call, getBytes);
        il.Emit(OpCodes.Leave, end);
        il.Append(cleanup);
        il.Emit(OpCodes.Callvirt, dispose);
        il.Emit(OpCodes.Endfinally);
        il.Append(end);
        fill.Body.ExceptionHandlers.Add(new ExceptionHandler(ExceptionHandlerType.Finally)
        {
            TryStart = begin, TryEnd = cleanup, HandlerStart = cleanup, HandlerEnd = end,
        });
    }
}
