using Mono.Cecil;
using Mono.Cecil.Cil;
using Xunit;

public sealed class ManagedPbkdf2Tests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void The_adapted_derivation_disposes_its_state_on_success_and_failure(bool fail)
    {
        using var module = ModuleDefinition.CreateModule($"PbkdfCleanup{Guid.NewGuid():N}", ModuleKind.Dll);
        var parent = AddType(module, "DeriveBytes", module.TypeSystem.Object);
        var count = new FieldDefinition("DisposeCount", FieldAttributes.Public | FieldAttributes.Static, module.TypeSystem.Int32);
        parent.Fields.Add(count);
        var parentConstructor = Constructor(parent, module.ImportReference(typeof(object).GetConstructor(Type.EmptyTypes)!));
        var dispose = Method(parent, "Dispose", false, module.TypeSystem.Void);
        var il = dispose.Body.GetILProcessor();
        il.Emit(OpCodes.Ldsfld, count);
        il.Emit(OpCodes.Ldc_I4_1);
        il.Emit(OpCodes.Add);
        il.Emit(OpCodes.Stsfld, count);
        il.Emit(OpCodes.Ret);
        var derive = AddType(module, "Rfc2898DeriveBytes", parent);
        var input = module.ImportReference(typeof(ReadOnlySpan<byte>));
        var output = module.ImportReference(typeof(Span<byte>));
        var algorithm = module.ImportReference(typeof(System.Security.Cryptography.HashAlgorithmName));
        Constructor(derive, parentConstructor, input, input, module.TypeSystem.Int32, algorithm);
        var getBytes = Method(derive, "GetBytes", false, module.TypeSystem.Void, output);
        il = getBytes.Body.GetILProcessor();
        if (fail)
        {
            il.Emit(OpCodes.Newobj, module.ImportReference(typeof(InvalidOperationException).GetConstructor(Type.EmptyTypes)!));
            il.Emit(OpCodes.Throw);
        }
        else il.Emit(OpCodes.Ret);
        var implementation = AddType(module, "Pbkdf2Implementation", module.TypeSystem.Object);
        var fill = Method(implementation, "Fill", true, module.TypeSystem.Void, input, input, module.TypeSystem.Int32, algorithm, output);
        fill.Body.GetILProcessor().Emit(OpCodes.Ret);
        var run = Method(implementation, "Run", true, module.TypeSystem.Void);
        run.Body.InitLocals = true;
        il = run.Body.GetILProcessor();
        foreach (var parameter in fill.Parameters)
        {
            var local = new VariableDefinition(parameter.ParameterType);
            run.Body.Variables.Add(local);
            il.Emit(OpCodes.Ldloc, local);
        }
        il.Emit(OpCodes.Call, fill);
        il.Emit(OpCodes.Ret);

        ManagedPbkdf2.Adapt(module);
        using var bytes = new MemoryStream();
        module.Write(bytes);
        var assembly = System.Reflection.Assembly.Load(bytes.ToArray());
        var entry = assembly.GetType(implementation.FullName)!.GetMethod("Run")!;
        if (fail)
        {
            var error = Assert.Throws<System.Reflection.TargetInvocationException>(() => entry.Invoke(null, null));
            Assert.True(error.InnerException is InvalidOperationException,
                "the adapted loop must preserve the backend failure");
        }
        else entry.Invoke(null, null);
        var actual = assembly.GetType(parent.FullName)!.GetField("DisposeCount")!.GetValue(null);
        Assert.True(actual is 1, "the generated finally must dispose exactly once on every exit");
    }

    [Fact]
    public void A_changed_runtime_signature_is_rejected_before_rewriting()
    {
        using var module = ModuleDefinition.CreateModule("ChangedRuntime", ModuleKind.Dll);
        var type = AddType(module, "Changed", module.TypeSystem.Object);
        Method(type, "Operation", false, module.TypeSystem.Int32, module.TypeSystem.String);
        Assert.Throws<InvalidOperationException>(() => RuntimeMethods.Require(type, "Operation", true,
            "System.Int32", "System.String"));
        Assert.Throws<InvalidOperationException>(() => RuntimeMethods.Require(type, "Operation", false,
            "System.Void", "System.String"));
        Assert.Throws<InvalidOperationException>(() => RuntimeMethods.Require(type, "Operation", false,
            "System.Int32", "System.Int32"));
        Assert.True(RuntimeMethods.Require(type, "Operation", false, "System.Int32", "System.String") is not null,
            "only the complete matching signature must be accepted");
    }

    [Fact]
    public void Generic_and_ambiguous_runtime_methods_are_rejected()
    {
        using var module = ModuleDefinition.CreateModule("AmbiguousRuntime", ModuleKind.Dll);
        var type = AddType(module, "Changed", module.TypeSystem.Object);
        var generic = Method(type, "Operation", false, module.TypeSystem.Void);
        generic.GenericParameters.Add(new GenericParameter("T", generic));
        Assert.Throws<InvalidOperationException>(() => RuntimeMethods.Require(type, "Operation", false, "System.Void"));
        generic.GenericParameters.Clear();
        Method(type, "Operation", false, module.TypeSystem.Void);
        Assert.Throws<InvalidOperationException>(() => RuntimeMethods.Require(type, "Operation", false, "System.Void"));
    }

    private static TypeDefinition AddType(ModuleDefinition module, string name, TypeReference parent)
    {
        var type = new TypeDefinition("System.Security.Cryptography", name, TypeAttributes.Public | TypeAttributes.Class, parent);
        module.Types.Add(type);
        return type;
    }

    private static MethodDefinition Constructor(TypeDefinition type, MethodReference parent, params TypeReference[] parameters)
    {
        var constructor = Method(type, ".ctor", false, type.Module.TypeSystem.Void, parameters);
        constructor.Attributes |= MethodAttributes.SpecialName | MethodAttributes.RTSpecialName;
        var il = constructor.Body.GetILProcessor();
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Call, parent);
        il.Emit(OpCodes.Ret);
        return constructor;
    }

    private static MethodDefinition Method(TypeDefinition type, string name, bool isStatic,
        TypeReference result, params TypeReference[] parameters)
    {
        var method = new MethodDefinition(name, MethodAttributes.Public | MethodAttributes.HideBySig |
            (isStatic ? MethodAttributes.Static : 0), result);
        foreach (var parameter in parameters) method.Parameters.Add(new ParameterDefinition(parameter));
        type.Methods.Add(method);
        return method;
    }
}
