using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class SocketPolling
{
    internal static void Adapt(ModuleDefinition module)
    {
        var engine = module.Types.Single(type => type.FullName == "System.Net.Sockets.SocketAsyncEngine");
        var hook = engine.Methods.Single(method => method.Name == "BeforePollHook" && method.Parameters.Count == 1);
        var calls = hook.Body.Instructions.Select(instruction => instruction.Operand).OfType<MethodReference>()
            .DistinctBy(method => method.FullName).ToArray();
        var listConstructor = calls.Single(method => method.Name == ".ctor" && method.Parameters.Count == 0
            && method.DeclaringType.FullName == "System.Collections.Generic.List`1<System.Int32>");
        var add = calls.Single(method => method.Name == "Add" && method.DeclaringType.FullName == listConstructor.DeclaringType.FullName);
        var closed = calls.Single(method => method.Name == "get_IsClosed");
        var handle = calls.Single(method => method.Name == "DangerousGetHandle");
        var context = module.Types.Single(type => type.FullName == "System.Net.Sockets.SocketAsyncContext");
        var socket = context.Fields.Single(field => field.Name == "_socket");
        var read = Import(module, engine, "wisp_socket_pollables", module.TypeSystem.Int32,
            module.TypeSystem.Int32, new PointerType(module.TypeSystem.IntPtr), new PointerType(module.TypeSystem.Int32));
        var free = Import(module, engine, "wisp_socket_pollables_free", module.TypeSystem.Void, module.TypeSystem.IntPtr);
        Rewrite(hook, socket, closed, handle, listConstructor, add, read, free);
    }

    private static MethodDefinition Import(ModuleDefinition module, TypeDefinition owner, string name, TypeReference result, params TypeReference[] parameters)
    {
        var library = module.ModuleReferences.SingleOrDefault(reference => reference.Name == "Wisp.Native");
        if (library is null)
        {
            library = new ModuleReference("Wisp.Native");
            module.ModuleReferences.Add(library);
        }
        var method = new MethodDefinition(name, MethodAttributes.Private | MethodAttributes.Static | MethodAttributes.PInvokeImpl, result)
        {
            ImplAttributes = MethodImplAttributes.PreserveSig,
            PInvokeInfo = new PInvokeInfo(PInvokeAttributes.CallConvCdecl, name, library)
        };
        foreach (var parameter in parameters) method.Parameters.Add(new ParameterDefinition(parameter));
        owner.Methods.Add(method);
        return method;
    }

    private static void Rewrite(MethodDefinition hook, FieldReference socket, MethodReference closed, MethodReference handle,
        MethodReference constructor, MethodReference add, MethodReference read, MethodReference free)
    {
        var module = hook.Module;
        hook.Body = new MethodBody(hook) { InitLocals = true };
        var context = Local(hook, socket.DeclaringType);
        var list = Local(hook, constructor.DeclaringType);
        var pointer = Local(hook, module.TypeSystem.IntPtr);
        var count = Local(hook, module.TypeSystem.Int32);
        var index = Local(hook, module.TypeSystem.Int32);
        var error = Local(hook, module.TypeSystem.Int32);
        var il = hook.Body.GetILProcessor();
        il.Emit(OpCodes.Newobj, constructor);
        il.Emit(OpCodes.Stloc, list);
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Castclass, context.VariableType);
        il.Emit(OpCodes.Stloc, context);
        il.Emit(OpCodes.Ldloc, context);
        il.Emit(OpCodes.Ldfld, socket);
        il.Emit(OpCodes.Callvirt, closed);
        var open = Instruction.Create(OpCodes.Ldloc, context);
        il.Emit(OpCodes.Brfalse, open);
        il.Emit(OpCodes.Ldloc, list);
        il.Emit(OpCodes.Ret);
        il.Append(open);
        il.Emit(OpCodes.Ldfld, socket);
        il.Emit(OpCodes.Callvirt, handle);
        il.Emit(OpCodes.Conv_I4);
        il.Emit(OpCodes.Ldloca, pointer);
        il.Emit(OpCodes.Conv_U);
        il.Emit(OpCodes.Ldloca, count);
        il.Emit(OpCodes.Conv_U);
        il.Emit(OpCodes.Call, read);
        il.Emit(OpCodes.Stloc, error);
        il.Emit(OpCodes.Ldloc, error);
        var success = Instruction.Create(OpCodes.Ldc_I4_0);
        il.Emit(OpCodes.Brfalse, success);
        EmitError(il, module, error);
        il.Append(success);
        il.Emit(OpCodes.Stloc, index);
        EmitList(hook, list, pointer, count, index, add, free);
    }

    private static VariableDefinition Local(MethodDefinition method, TypeReference type)
    {
        var local = new VariableDefinition(type);
        method.Body.Variables.Add(local);
        return local;
    }

    private static void EmitError(ILProcessor il, ModuleDefinition module, VariableDefinition error)
    {
        var toString = new MethodReference("ToString", module.TypeSystem.String, module.TypeSystem.Int32) { HasThis = true };
        var concat = new MethodReference("Concat", module.TypeSystem.String, module.TypeSystem.String);
        concat.Parameters.Add(new ParameterDefinition(module.TypeSystem.String));
        concat.Parameters.Add(new ParameterDefinition(module.TypeSystem.String));
        var exception = new TypeReference("System.IO", "IOException", module, module.TypeSystem.CoreLibrary);
        var constructor = new MethodReference(".ctor", module.TypeSystem.Void, exception) { HasThis = true };
        constructor.Parameters.Add(new ParameterDefinition(module.TypeSystem.String));
        il.Emit(OpCodes.Ldstr, "native socket poll failed with errno ");
        il.Emit(OpCodes.Ldloca, error);
        il.Emit(OpCodes.Call, toString);
        il.Emit(OpCodes.Call, concat);
        il.Emit(OpCodes.Newobj, constructor);
        il.Emit(OpCodes.Throw);
    }

    private static void EmitList(MethodDefinition hook, VariableDefinition list, VariableDefinition pointer,
        VariableDefinition count, VariableDefinition index, MethodReference add, MethodReference free)
    {
        var il = hook.Body.GetILProcessor();
        var check = Instruction.Create(OpCodes.Ldloc, index);
        var start = Instruction.Create(OpCodes.Br, check);
        var loop = Instruction.Create(OpCodes.Ldloc, list);
        var cleanup = Instruction.Create(OpCodes.Ldloc, pointer);
        var done = Instruction.Create(OpCodes.Ldloc, list);
        il.Append(start);
        il.Append(loop);
        il.Emit(OpCodes.Ldloc, pointer);
        il.Emit(OpCodes.Ldloc, index);
        il.Emit(OpCodes.Ldc_I4_4);
        il.Emit(OpCodes.Mul);
        il.Emit(OpCodes.Conv_I);
        il.Emit(OpCodes.Add);
        il.Emit(OpCodes.Ldind_I4);
        il.Emit(OpCodes.Callvirt, add);
        il.Emit(OpCodes.Ldloc, index);
        il.Emit(OpCodes.Ldc_I4_1);
        il.Emit(OpCodes.Add);
        il.Emit(OpCodes.Stloc, index);
        il.Append(check);
        il.Emit(OpCodes.Ldloc, count);
        il.Emit(OpCodes.Blt, loop);
        il.Emit(OpCodes.Leave, done);
        il.Append(cleanup);
        il.Emit(OpCodes.Call, free);
        il.Emit(OpCodes.Endfinally);
        il.Append(done);
        il.Emit(OpCodes.Ret);
        hook.Body.ExceptionHandlers.Add(new ExceptionHandler(ExceptionHandlerType.Finally)
        {
            TryStart = start, TryEnd = cleanup, HandlerStart = cleanup, HandlerEnd = done
        });
    }
}
