using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class CertificateValues
{
    private const string CertificateNamespace = "System.Security.Cryptography.X509Certificates";

    internal static void Adapt(ModuleDefinition module, ModuleDefinition sdk)
    {
        var pal = Find(module, "ICertificatePal");
        var core = Find(module, "ICertificatePalCore");
        var bridge = sdk.Types.Single(t => t.FullName == "Wisp.Runtime.CertificateValue");
        RuntimeMethods.Require(core, "get_RawData", false, "System.Byte[]");
        RuntimeMethods.Require(core, "get_SignatureAlgorithm", false, "System.String");
        var type = new TypeDefinition(CertificateNamespace, "WispCertificateValuePal",
            TypeAttributes.NotPublic | TypeAttributes.Sealed | TypeAttributes.BeforeFieldInit,
            module.TypeSystem.Object);
        module.Types.Add(type);
        type.Interfaces.Add(new InterfaceImplementation(pal));
        var field = new FieldDefinition("_value", FieldAttributes.Private | FieldAttributes.InitOnly,
            module.ImportReference(bridge));
        type.Fields.Add(field);
        var constructor = AddConstructor(module, type, field, bridge);
        foreach (var method in pal.Methods.Concat(core.Methods))
            Implement(module, type, field, bridge, method);
        var dispose = module.ImportReference(typeof(IDisposable).GetMethod(nameof(IDisposable.Dispose))
            ?? throw new InvalidOperationException("Missing IDisposable.Dispose contract."));
        Implement(module, type, field, bridge, dispose);
        AdaptFactories(module, bridge, constructor, pal);
    }

    private static TypeDefinition Find(ModuleDefinition module, string name) =>
        module.Types.Single(type => type.FullName == CertificateNamespace + "." + name);

    private static MethodDefinition AddConstructor(ModuleDefinition module, TypeDefinition type,
        FieldDefinition field, TypeDefinition bridge)
    {
        var constructor = new MethodDefinition(".ctor", MethodAttributes.Public | MethodAttributes.SpecialName |
            MethodAttributes.RTSpecialName | MethodAttributes.HideBySig, module.TypeSystem.Void);
        constructor.Parameters.Add(new ParameterDefinition(module.ImportReference(bridge)));
        type.Methods.Add(constructor);
        var il = constructor.Body.GetILProcessor();
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Call, module.ImportReference(typeof(object).GetConstructor(Type.EmptyTypes)
            ?? throw new InvalidOperationException("Missing object constructor.")));
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Ldarg_1);
        il.Emit(OpCodes.Stfld, field);
        il.Emit(OpCodes.Ret);
        return constructor;
    }

    private static void AdaptFactories(ModuleDefinition module, TypeDefinition bridge,
        MethodDefinition constructor, TypeDefinition pal)
    {
        var factory = Find(module, "CertificatePal");
        var fromBlob = RuntimeMethods.Require(factory, "FromBlob", true, pal.FullName,
            "System.ReadOnlySpan`1<System.Byte>", "Microsoft.Win32.SafeHandles.SafePasswordHandle",
            CertificateNamespace + ".X509KeyStorageFlags");
        ReplaceParse(module, fromBlob, bridge, constructor, fromBlob.Parameters[2]);
        var load = RuntimeMethods.Require(Find(module, "X509CertificateLoader"), "LoadCertificatePal", true,
            pal.FullName, "System.ReadOnlySpan`1<System.Byte>");
        ReplaceParse(module, load, bridge, constructor, null);
        var fromHandle = RuntimeMethods.Require(factory, "FromHandle", true, pal.FullName, "System.IntPtr");
        fromHandle.Body = new MethodBody(fromHandle);
        Reject(module, fromHandle.Body.GetILProcessor(), "Certificate native handle import is unsupported.");
        var copy = RuntimeMethods.Require(factory, "FromOtherCert", true, pal.FullName,
            CertificateNamespace + ".X509Certificate");
        copy.Body = new MethodBody(copy);
        var il = copy.Body.GetILProcessor();
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Callvirt, RuntimeMethods.Require(Find(module, "X509Certificate"),
            "GetRawCertData", false, "System.Byte[]"));
        il.Emit(OpCodes.Call, module.ImportReference(RuntimeMethods.Require(bridge, "ParseArray", true,
            bridge.FullName, "System.Byte[]")));
        il.Emit(OpCodes.Newobj, constructor);
        il.Emit(OpCodes.Ret);
    }

    private static void ReplaceParse(ModuleDefinition module, MethodDefinition method,
        TypeDefinition bridge, MethodDefinition constructor, ParameterDefinition? keyStorageFlags)
    {
        method.Body = new MethodBody(method);
        var il = method.Body.GetILProcessor();
        if (keyStorageFlags is not null)
        {
            var valid = Instruction.Create(OpCodes.Nop);
            il.Emit(OpCodes.Ldarg, keyStorageFlags);
            il.Emit(OpCodes.Brfalse, valid);
            Reject(module, il, "Certificate key storage flags are unsupported by the DER value adapter.");
            il.Append(valid);
        }
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Call, module.ImportReference(RuntimeMethods.Require(bridge, "Parse", true,
            bridge.FullName, "System.ReadOnlySpan`1<System.Byte>")));
        il.Emit(OpCodes.Newobj, constructor);
        il.Emit(OpCodes.Ret);
    }

    private static void Implement(ModuleDefinition module, TypeDefinition type, FieldDefinition field,
        TypeDefinition bridge, MethodReference original)
    {
        var method = new MethodDefinition(original.Name, MethodAttributes.Public | MethodAttributes.Virtual |
            MethodAttributes.Final | MethodAttributes.NewSlot | MethodAttributes.HideBySig,
            module.ImportReference(original.ReturnType));
        foreach (var parameter in original.Parameters)
            method.Parameters.Add(new ParameterDefinition(module.ImportReference(parameter.ParameterType)));
        method.Overrides.Add(original);
        type.Methods.Add(method);
        var il = method.Body.GetILProcessor();
        if (original.Name is "get_RawData" or "get_SignatureAlgorithm" or "Dispose")
        {
            var target = RuntimeMethods.Require(bridge, original.Name, false, original.ReturnType.FullName,
                original.Parameters.Select(parameter => parameter.ParameterType.FullName).ToArray());
            il.Emit(OpCodes.Ldarg_0);
            il.Emit(OpCodes.Ldfld, field);
            il.Emit(OpCodes.Callvirt, module.ImportReference(target));
            il.Emit(OpCodes.Ret);
        }
        else Reject(module, il, "Certificate PAL operation unsupported: " + original.FullName);
    }

    private static void Reject(ModuleDefinition module, ILProcessor il, string reason)
    {
        il.Emit(OpCodes.Ldstr, reason);
        il.Emit(OpCodes.Newobj, module.ImportReference(typeof(PlatformNotSupportedException)
            .GetConstructor(new[] { typeof(string) }) ?? throw new InvalidOperationException("Missing exception constructor.")));
        il.Emit(OpCodes.Throw);
    }
}
