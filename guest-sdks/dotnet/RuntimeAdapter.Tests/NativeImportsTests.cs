using System.Runtime.Loader;
using Mono.Cecil;
using Mono.Cecil.Cil;
using Xunit;

public sealed class NativeImportsTests
{
    [Fact]
    public void An_adapted_timer_import_can_call_its_helper_from_another_type()
    {
        using var module = ModuleDefinition.CreateModule("AdaptedTimer", ModuleKind.Dll);
        var owner = AddType(module, "HelperOwner");
        var caller = AddType(module, "TimerCaller");
        var native = new ModuleReference("wasi:clocks/monotonic-clock@0.2.0");
        module.ModuleReferences.Add(native);
        var subscribe = new MethodDefinition("SubscribeDuration",
            MethodAttributes.Public | MethodAttributes.Static | MethodAttributes.PInvokeImpl,
            module.TypeSystem.Int32)
        {
            ImplAttributes = MethodImplAttributes.PreserveSig,
            PInvokeInfo = new PInvokeInfo(PInvokeAttributes.CallConvCdecl, "subscribe-duration", native)
        };
        subscribe.Parameters.Add(new ParameterDefinition(module.TypeSystem.Int64));
        subscribe.CustomAttributes.Add(new CustomAttribute(module.ImportReference(
            typeof(WasmImportLinkageAttribute).GetConstructor(Type.EmptyTypes)!)));
        caller.Methods.Add(subscribe);

        NativeImports.Adapt(module, owner);
        var helper = owner.Methods.Single();
        // Replace only the host operation, so executing the rewritten caller still enforces CLR access checks.
        helper.PInvokeInfo = null;
        helper.IsPInvokeImpl = false;
        helper.ImplAttributes = MethodImplAttributes.IL | MethodImplAttributes.Managed;
        helper.Body = new MethodBody(helper);
        helper.Body.GetILProcessor().Emit(OpCodes.Ldc_I8, 73L);
        helper.Body.GetILProcessor().Emit(OpCodes.Ret);

        using var bytes = new MemoryStream();
        module.Write(bytes);
        bytes.Position = 0;
        var context = new AssemblyLoadContext("adapted-timer", isCollectible: true);
        try
        {
            var assembly = context.LoadFromStream(bytes);
            var method = assembly.GetType("Fixture.TimerCaller")!.GetMethod("SubscribeDuration")!;
            var result = method.Invoke(null, [1L]);
            Assert.True(result is 73,
                "a rewritten timer import must reach its same-assembly helper across declaring types");
        }
        finally { context.Unload(); }
    }

    private static TypeDefinition AddType(ModuleDefinition module, string name)
    {
        var type = new TypeDefinition("Fixture", name, TypeAttributes.Public, module.TypeSystem.Object);
        module.Types.Add(type);
        return type;
    }

    public sealed class WasmImportLinkageAttribute : Attribute;
}
