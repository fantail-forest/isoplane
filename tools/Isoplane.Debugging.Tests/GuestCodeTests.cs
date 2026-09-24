using System.CodeDom.Compiler;
using System.Diagnostics;
using System.Runtime.CompilerServices;
using Mono.Cecil;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class GuestCodeTests
{
    [Fact]
    public void the_package_location_comes_from_runtime_metadata()
    {
        using var module = ModuleDefinition.CreateModule("Runtime", ModuleKind.Dll);
        var loader = new TypeDefinition("Isoplane.MonoRuntime", "PackageLoader", TypeAttributes.Public, module.TypeSystem.Object);
        loader.Fields.Add(new FieldDefinition("Root", FieldAttributes.Public | FieldAttributes.Static | FieldAttributes.Literal,
            module.TypeSystem.String) { Constant = "/fixture/package" });
        module.Types.Add(loader);
        Assert.True(GuestCode.PackageRoot(module) == "/fixture/package/", "The debugger must use the runtime's package location with a directory boundary.");
    }

    [Fact]
    public void a_runtime_without_package_metadata_is_rejected()
    {
        using var module = ModuleDefinition.CreateModule("Runtime", ModuleKind.Dll);
        Assert.Throws<ProtocolException>(() => GuestCode.PackageRoot(module));
    }

    [Theory]
    [InlineData(typeof(GeneratedCodeAttribute))]
    [InlineData(typeof(DebuggerNonUserCodeAttribute))]
    [InlineData(typeof(DebuggerHiddenAttribute))]
    public void explicitly_hidden_methods_are_not_customer_frames(Type attribute)
    {
        using var module = ModuleDefinition.CreateModule("Customer", ModuleKind.Dll);
        var method = Method(module);
        method.CustomAttributes.Add(Attribute(module, attribute));
        Assert.True(GuestCode.Hidden(method), "An explicitly hidden method must not appear in the customer stack.");
    }

    [Fact]
    public void nested_continuations_in_generated_infrastructure_remain_hidden()
    {
        using var module = ModuleDefinition.CreateModule("Customer", ModuleKind.Dll);
        var method = Method(module);
        var generated = new TypeDefinition("Example", "Routing", TypeAttributes.Public, module.TypeSystem.Object);
        module.Types.Add(generated);
        generated.CustomAttributes.Add(Attribute(module, typeof(GeneratedCodeAttribute)));
        module.Types.Remove(method.DeclaringType);
        method.DeclaringType.Attributes = TypeAttributes.NestedPrivate;
        generated.NestedTypes.Add(method.DeclaringType);
        Assert.True(GuestCode.Hidden(method), "A continuation nested inside generated routing code must remain hidden.");
    }

    [Fact]
    public void customer_continuations_are_visible_even_with_framework_like_names()
    {
        using var module = ModuleDefinition.CreateModule("Customer", ModuleKind.Dll);
        var method = Method(module);
        method.DeclaringType.CustomAttributes.Add(Attribute(module, typeof(CompilerGeneratedAttribute)));
        Assert.False(GuestCode.Hidden(method), "Compiler-generated async state and customer namespaces must not hide customer code.");
    }

    private static MethodDefinition Method(ModuleDefinition module)
    {
        var type = new TypeDefinition("System.Customer", "Continuation", TypeAttributes.Public, module.TypeSystem.Object);
        module.Types.Add(type);
        var method = new MethodDefinition("MoveNext", MethodAttributes.Public, module.TypeSystem.Void);
        type.Methods.Add(method);
        return method;
    }

    private static CustomAttribute Attribute(ModuleDefinition module, Type attribute) =>
        new(module.ImportReference(attribute.GetConstructors().Single()));
}
