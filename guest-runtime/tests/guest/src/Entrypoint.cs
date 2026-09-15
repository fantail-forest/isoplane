using System.Runtime.InteropServices;
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Acceptance.AcceptanceHandler>>;

/// <summary>
/// The module's five core exports in either profile. Under NativeAOT the
/// methods carry <c>[UnmanagedCallersOnly]</c> and become the wasm exports;
/// under Mono the SDK's C entry looks this class up by name from the global
/// namespace and invokes the same methods through the runtime, with
/// <c>Main</c> rooting them through trimming.
/// </summary>
internal static class WispGeneratedExports
{
#if WISP_MONO
    [System.Diagnostics.CodeAnalysis.DynamicDependency(
        System.Diagnostics.CodeAnalysis.DynamicallyAccessedMemberTypes.PublicMethods,
        typeof(WispGeneratedExports))]
    public static void Main() { }
#endif

#if !WISP_MONO
    [UnmanagedCallersOnly(EntryPoint = "wisp_alloc")]
#endif
    public static int Alloc(int size, int align) => Guest.Alloc(size, align);

#if !WISP_MONO
    [UnmanagedCallersOnly(EntryPoint = "wisp_handle")]
#endif
    public static long Handle(int ptr, int len) => Guest.Handle(ptr, len);

#if !WISP_MONO
    [UnmanagedCallersOnly(EntryPoint = "wisp_wake")]
#endif
    public static long Wake(long future, int ptr, int len) => Guest.Wake(future, ptr, len);

#if !WISP_MONO
    [UnmanagedCallersOnly(EntryPoint = "wisp_initialize")]
#endif
    public static long Initialize() => Guest.Initialize();

#if !WISP_MONO
    [UnmanagedCallersOnly(EntryPoint = "wisp_gc_hint")]
#endif
    public static void GcHint(int pressure) => Guest.GcHint(pressure);
}
