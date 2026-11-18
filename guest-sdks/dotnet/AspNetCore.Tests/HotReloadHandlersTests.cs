using System.Reflection;
using System.Reflection.Emit;
using System.Reflection.Metadata;
using System.Runtime.Loader;
using Isoplane.AspNetCore.Tests;
using Isoplane.Runtime;
using Xunit;

[assembly: MetadataUpdateHandler(typeof(HotReloadHandlersTests.Declared))]

namespace Isoplane.AspNetCore.Tests;

public sealed class HotReloadHandlersTests : IDisposable
{
    private static readonly List<string> Calls = [];
    private static readonly Type[] Updated = [typeof(HotReloadHandlersTests), typeof(string)];
    private readonly AssemblyLoadContext emitted = new(nameof(HotReloadHandlersTests), isCollectible: true);

    public HotReloadHandlersTests() => Calls.Clear();

    public void Dispose() => emitted.Unload();

    [Fact]
    public void A_handler_the_compiler_attributed_receives_the_updated_types_in_both_methods()
    {
        string[] warnings = HotReloadHandlers.Notify([typeof(HotReloadHandlersTests).Assembly], Updated);

        Assert.Empty(warnings);
        Assert.Equal(["Declared.ClearCache(HotReloadHandlersTests,String)", "Declared.UpdateApplication(HotReloadHandlersTests,String)"], Calls);
    }

    [Fact]
    public void Every_cache_is_cleared_before_any_application_update_and_referenced_assemblies_go_first()
    {
        var lower = Declaring(typeof(Lower));
        var upper = Declaring(typeof(Upper), lower);

        string[] warnings = HotReloadHandlers.Notify([upper, lower], Updated);

        Assert.Empty(warnings);
        Assert.Equal(["Lower.ClearCache", "Upper.ClearCache", "Lower.UpdateApplication", "Upper.UpdateApplication"], Calls);
    }

    [Fact]
    public void A_handler_with_one_method_is_called_without_a_warning()
    {
        string[] warnings = HotReloadHandlers.Notify([Declaring(typeof(ClearsOnly)), Declaring(typeof(UpdatesOnly))], Updated);

        Assert.Empty(warnings);
        Assert.Equal(["ClearsOnly.ClearCache", "UpdatesOnly.UpdateApplication"], Calls);
    }

    [Fact]
    public void A_handler_with_neither_method_is_reported_by_name()
    {
        string warning = Assert.Single(HotReloadHandlers.Notify([Declaring(typeof(Empty))], Updated));

        Assert.Contains(typeof(Empty).FullName!, warning);
        Assert.Contains("declares neither", warning);
    }

    [Fact]
    public void A_handler_for_static_content_alone_is_neither_called_nor_reported()
    {
        Assert.Empty(HotReloadHandlers.Notify([Declaring(typeof(ContentOnly))], Updated));
        Assert.Empty(Calls);
    }

    [Fact]
    public void A_method_of_the_wrong_shape_is_reported_and_the_other_method_still_runs()
    {
        string[] warnings = HotReloadHandlers.Notify([Declaring(typeof(Misdeclared))], Updated);

        string warning = Assert.Single(warnings);
        Assert.Contains(typeof(Misdeclared).FullName!, warning);
        Assert.Contains("Int32 ClearCache(System.Type[])", warning);
        Assert.Equal(["Misdeclared.UpdateApplication"], Calls);
    }

    [Fact]
    public void A_handler_that_throws_is_reported_with_its_exception_and_the_remaining_handlers_run()
    {
        var throwing = Declaring(typeof(Throwing));
        var later = Declaring(typeof(Lower), throwing);

        string[] warnings = HotReloadHandlers.Notify([later, throwing], Updated);

        string warning = Assert.Single(warnings);
        Assert.Contains(typeof(Throwing).FullName + ".ClearCache", warning);
        Assert.Contains("System.InvalidOperationException: stale cache", warning);
        Assert.Equal(["Lower.ClearCache", "Throwing.UpdateApplication", "Lower.UpdateApplication"], Calls);
    }

    [Fact]
    public void Several_handlers_in_one_assembly_run_in_declaration_order()
    {
        string[] warnings = HotReloadHandlers.Notify([Declaring([typeof(Upper), typeof(Lower)])], Updated);

        Assert.Empty(warnings);
        Assert.Equal(["Upper.ClearCache", "Lower.ClearCache", "Upper.UpdateApplication", "Lower.UpdateApplication"], Calls);
    }

    private Assembly Declaring(Type handler, params Assembly[] references) => Declaring([handler], references);

    /// <summary>Emits and loads an assembly that declares the handlers and references the given assemblies.</summary>
    private Assembly Declaring(Type[] handlers, params Assembly[] references)
    {
        var builder = new PersistedAssemblyBuilder(new AssemblyName("Handlers" + Guid.NewGuid().ToString("N")), typeof(object).Assembly);
        var constructor = typeof(MetadataUpdateHandlerAttribute).GetConstructor([typeof(Type)])!;
        foreach (var handler in handlers) builder.SetCustomAttribute(new CustomAttributeBuilder(constructor, [handler]));
        var marker = builder.DefineDynamicModule("Handlers").DefineType("Marker", TypeAttributes.Public);
        for (int index = 0; index < references.Length; index++)
            marker.DefineField("reference" + index, references[index].GetType("Marker", throwOnError: true)!, FieldAttributes.Public | FieldAttributes.Static);
        marker.CreateType();
        using var image = new MemoryStream();
        builder.Save(image);
        image.Position = 0;
        return emitted.LoadFromStream(image);
    }

    private static void Record(Type handler, string method) => Calls.Add(handler.Name + "." + method);

    internal static class Declared
    {
        internal static void ClearCache(Type[]? updatedTypes) => Record(updatedTypes);
        internal static void UpdateApplication(Type[]? updatedTypes) => Record(updatedTypes, nameof(UpdateApplication));

        private static void Record(Type[]? updatedTypes, string method = nameof(ClearCache)) =>
            Calls.Add($"{nameof(Declared)}.{method}({string.Join(',', updatedTypes!.Select(type => type.Name))})");
    }

    public static class Lower
    {
        public static void ClearCache(Type[]? updatedTypes) => Record(typeof(Lower), nameof(ClearCache));
        public static void UpdateApplication(Type[]? updatedTypes) => Record(typeof(Lower), nameof(UpdateApplication));
    }

    public static class Upper
    {
        public static void ClearCache(Type[]? updatedTypes) => Record(typeof(Upper), nameof(ClearCache));
        public static void UpdateApplication(Type[]? updatedTypes) => Record(typeof(Upper), nameof(UpdateApplication));
    }

    public static class ClearsOnly
    {
        private static void ClearCache(Type[]? updatedTypes) => Record(typeof(ClearsOnly), nameof(ClearCache));
    }

    public static class UpdatesOnly
    {
        internal static void UpdateApplication(Type[]? updatedTypes) => Record(typeof(UpdatesOnly), nameof(UpdateApplication));
    }

    public static class Empty;

    public static class ContentOnly
    {
        public static void UpdateContent(string assemblyName, bool isApplicationProject, string relativePath, byte[] contents) =>
            Record(typeof(ContentOnly), nameof(UpdateContent));
    }

    public static class Misdeclared
    {
        public static int ClearCache(Type[]? updatedTypes)
        {
            Record(typeof(Misdeclared), nameof(ClearCache));
            return 0;
        }

        public static void UpdateApplication(Type[]? updatedTypes) => Record(typeof(Misdeclared), nameof(UpdateApplication));
    }

    public static class Throwing
    {
        public static void ClearCache(Type[]? updatedTypes) => throw new InvalidOperationException("stale cache");
        public static void UpdateApplication(Type[]? updatedTypes) => Record(typeof(Throwing), nameof(UpdateApplication));
    }
}
