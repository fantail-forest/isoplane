using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class NativeImports
{
    internal static void Adapt(ModuleDefinition module, TypeDefinition owner)
    {
        var entries = ReadMap();
        var imports = new Dictionary<string, MethodDefinition>();
        foreach (var method in module.GetTypes().SelectMany(type => type.Methods).ToArray())
        {
            var native = method.PInvokeInfo;
            if (native is null || !entries.TryGetValue((native.Module.Name, native.EntryPoint), out var entry))
                continue;
            var marker = method.CustomAttributes.Single(attribute => attribute.AttributeType.Name == "WasmImportLinkageAttribute");
            if (!imports.TryGetValue(entry.Capability, out var target))
            {
                target = CreateImport(module, owner, marker, entry.Capability);
                imports.Add(entry.Capability, target);
            }
            method.CustomAttributes.Remove(marker);
            Rewrite(method, target, entry.Operation);
        }
    }

    private sealed record Entry(string Capability, int Operation);

    private static Dictionary<(string, string), Entry> ReadMap()
    {
        using var source = typeof(NativeImports).Assembly.GetManifestResourceStream("RuntimeAdapter.NativeImports.tsv")
            ?? throw new InvalidOperationException("The generated native import map is missing.");
        using var reader = new StreamReader(source);
        return reader.ReadToEnd().Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .Select(line => line.Split('\t')).ToDictionary(fields => (fields[0], fields[1]),
                fields => new Entry(fields[2], int.Parse(fields[3], System.Globalization.CultureInfo.InvariantCulture)));
    }

    private static MethodDefinition CreateImport(ModuleDefinition module, TypeDefinition owner, CustomAttribute marker, string capability)
    {
        var target = new MethodDefinition("Wisp_" + capability,
            MethodAttributes.Private | MethodAttributes.Static | MethodAttributes.PInvokeImpl, module.TypeSystem.Int64)
        {
            ImplAttributes = MethodImplAttributes.PreserveSig
        };
        target.Parameters.Add(new ParameterDefinition(module.TypeSystem.Int32));
        target.Parameters.Add(new ParameterDefinition(module.TypeSystem.IntPtr));
        target.Parameters.Add(new ParameterDefinition(module.TypeSystem.Int32));
        var host = module.ModuleReferences.SingleOrDefault(reference => reference.Name == "wisp");
        if (host is null)
        {
            host = new ModuleReference("wisp");
            module.ModuleReferences.Add(host);
        }
        target.PInvokeInfo = new PInvokeInfo(PInvokeAttributes.CallConvCdecl, capability, host);
        target.CustomAttributes.Add(new CustomAttribute(marker.Constructor));
        owner.Methods.Add(target);
        return target;
    }

    private static void Rewrite(MethodDefinition method, MethodDefinition target, int operation)
    {
        method.PInvokeInfo = null;
        method.IsPInvokeImpl = false;
        method.ImplAttributes = MethodImplAttributes.IL | MethodImplAttributes.Managed;
        method.Body = new MethodBody(method);
        var pointer = new VariableDefinition(method.Module.TypeSystem.IntPtr);
        method.Body.Variables.Add(pointer);
        var il = method.Body.GetILProcessor();
        EmitArguments(method, il, pointer);
        il.Emit(OpCodes.Ldc_I4, operation);
        il.Emit(OpCodes.Ldloc, pointer);
        il.Emit(OpCodes.Ldc_I4, method.Parameters.Count);
        il.Emit(OpCodes.Call, target);
        switch (method.ReturnType.MetadataType)
        {
            case MetadataType.Void: il.Emit(OpCodes.Pop); break;
            case MetadataType.Int32: il.Emit(OpCodes.Conv_I4); break;
            case MetadataType.Int64: break;
            case MetadataType.IntPtr: il.Emit(OpCodes.Conv_I); break;
            default: throw new InvalidOperationException("Unsupported native import return type: " + method.FullName);
        }
        il.Emit(OpCodes.Ret);
    }

    private static void EmitArguments(MethodDefinition method, ILProcessor il, VariableDefinition pointer)
    {
        il.Emit(OpCodes.Ldc_I4, Math.Max(1, method.Parameters.Count) * sizeof(ulong));
        il.Emit(OpCodes.Conv_U);
        il.Emit(OpCodes.Localloc);
        il.Emit(OpCodes.Stloc, pointer);
        for (var index = 0; index < method.Parameters.Count; index++)
        {
            il.Emit(OpCodes.Ldloc, pointer);
            il.Emit(OpCodes.Ldc_I4, index * sizeof(ulong));
            il.Emit(OpCodes.Add);
            il.Emit(OpCodes.Ldarg, method.Parameters[index]);
            il.Emit(OpCodes.Conv_U8);
            il.Emit(OpCodes.Stind_I8);
        }
    }
}
