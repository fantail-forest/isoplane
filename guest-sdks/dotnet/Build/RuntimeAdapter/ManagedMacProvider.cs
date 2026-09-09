using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class ManagedMacProvider
{
    internal static void Adapt(ModuleDefinition module, ModuleDefinition sdk)
    {
        var baseType = module.Types.Single(t => t.FullName == "System.Security.Cryptography.HashProvider");
        ValidateContract(baseType);
        var bridge = sdk.Types.Single(t => t.FullName == "Wisp.Runtime.ManagedHmac");
        var provider = new TypeDefinition("System.Security.Cryptography", "WispHmacProvider",
            TypeAttributes.NotPublic | TypeAttributes.Sealed | TypeAttributes.BeforeFieldInit, baseType);
        module.Types.Add(provider);
        var state = new FieldDefinition("_state", FieldAttributes.Private | FieldAttributes.InitOnly,
            module.ImportReference(bridge));
        provider.Fields.Add(state);
        var constructor = AddConstructor(module, provider, state, bridge, baseType);
        foreach (var original in baseType.Methods.Where(method => method.IsAbstract))
            AddOverride(module, provider, state, bridge, original);
        AdaptFactory(module, baseType, constructor);
    }

    private static void ValidateContract(TypeDefinition baseType)
    {
        RuntimeMethods.Require(baseType, "AppendHashData", false, "System.Void", "System.ReadOnlySpan`1<System.Byte>");
        RuntimeMethods.Require(baseType, "Clone", false, baseType.FullName);
        RuntimeMethods.Require(baseType, "FinalizeHashAndReset", false, "System.Int32", "System.Span`1<System.Byte>");
        RuntimeMethods.Require(baseType, "GetCurrentHash", false, "System.Int32", "System.Span`1<System.Byte>");
        RuntimeMethods.Require(baseType, "get_HashSizeInBytes", false, "System.Int32");
        RuntimeMethods.Require(baseType, "Dispose", false, "System.Void", "System.Boolean");
        RuntimeMethods.Require(baseType, "Reset", false, "System.Void");
    }

    private static MethodDefinition AddConstructor(ModuleDefinition module, TypeDefinition provider,
        FieldDefinition state, TypeDefinition bridge, TypeDefinition baseType)
    {
        var constructor = new MethodDefinition(".ctor", MethodAttributes.Public | MethodAttributes.HideBySig |
            MethodAttributes.SpecialName | MethodAttributes.RTSpecialName, module.TypeSystem.Void);
        var bridgeCtor = RuntimeMethods.Require(bridge, ".ctor", false, "System.Void", "System.ReadOnlySpan`1<System.Byte>");
        constructor.Parameters.Add(new ParameterDefinition(module.ImportReference(bridgeCtor.Parameters[0].ParameterType)));
        provider.Methods.Add(constructor);
        var il = constructor.Body.GetILProcessor();
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Call, RuntimeMethods.Require(baseType, ".ctor", false, "System.Void"));
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Ldarg_1);
        il.Emit(OpCodes.Newobj, module.ImportReference(bridgeCtor));
        il.Emit(OpCodes.Stfld, state);
        il.Emit(OpCodes.Ret);
        return constructor;
    }

    private static void AddOverride(ModuleDefinition module, TypeDefinition provider, FieldDefinition state,
        TypeDefinition bridge, MethodDefinition original)
    {
        var implementation = new MethodDefinition(original.Name,
            (original.Attributes & MethodAttributes.MemberAccessMask) | MethodAttributes.Virtual |
            MethodAttributes.HideBySig, module.ImportReference(original.ReturnType));
        foreach (var argument in original.Parameters)
            implementation.Parameters.Add(new ParameterDefinition(argument.Name, argument.Attributes,
                module.ImportReference(argument.ParameterType)));
        implementation.Overrides.Add(original);
        provider.Methods.Add(implementation);
        var il = implementation.Body.GetILProcessor();
        if (original.Name is "Clone" or "GetCurrentHash")
        {
            Reject(module, il, $"The managed SHA256 HMAC backend does not support {original.Name}.");
            return;
        }
        var target = RuntimeMethods.Require(bridge, original.Name, false, original.ReturnType.FullName,
            original.Parameters.Select(parameter => parameter.ParameterType.FullName).ToArray());
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Ldfld, state);
        foreach (var argument in implementation.Parameters) il.Emit(OpCodes.Ldarg, argument);
        il.Emit(OpCodes.Callvirt, module.ImportReference(target));
        il.Emit(OpCodes.Ret);
    }

    private static void AdaptFactory(ModuleDefinition module, TypeDefinition baseType, MethodDefinition constructor)
    {
        var dispenser = module.Types.Single(t => t.FullName == "System.Security.Cryptography.HashProviderDispenser");
        var factory = RuntimeMethods.Require(dispenser, "CreateMacProvider", true, baseType.FullName,
            "System.String", "System.ReadOnlySpan`1<System.Byte>");
        factory.Body = new MethodBody(factory);
        var il = factory.Body.GetILProcessor();
        var supported = Instruction.Create(OpCodes.Ldarg_1);
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Ldstr, "SHA256");
        il.Emit(OpCodes.Call, module.ImportReference(typeof(string).GetMethod("op_Equality", new[] { typeof(string), typeof(string) }) ?? throw new InvalidOperationException("Missing string equality operator.")));
        il.Emit(OpCodes.Brtrue, supported);
        Reject(module, il, "The managed HMAC backend supports SHA256 only.");
        il.Append(supported);
        il.Emit(OpCodes.Newobj, constructor);
        il.Emit(OpCodes.Ret);
    }

    private static void Reject(ModuleDefinition module, ILProcessor il, string reason)
    {
        il.Emit(OpCodes.Ldstr, reason);
        il.Emit(OpCodes.Newobj, module.ImportReference(typeof(PlatformNotSupportedException)
            .GetConstructor(new[] { typeof(string) }) ?? throw new InvalidOperationException("Missing exception constructor.")));
        il.Emit(OpCodes.Throw);
    }
}
