using System.Runtime.InteropServices;

using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.Api>>;

namespace Wisp;

/// <summary>
/// This module's five core-wasm exports, each delegating to the SDK's
/// generic implementation. The `using Guest = …` alias above is the whole
/// declaration; it is the C# analogue of the Rust SDK's
/// <c>export_handler!(Api)</c>.
///
/// The methods are boilerplate, and exist only because
/// <c>[UnmanagedCallersOnly]</c> cannot be applied to a method of a generic
/// class while NativeAOT-LLVM needs the attribute to emit a wasm export under
/// the exact name the host linker looks for. A finished SDK would source-
/// generate this file from the alias.
/// </summary>
internal static class Entrypoint
{
    [UnmanagedCallersOnly(EntryPoint = "wisp_alloc")]
    public static int Alloc(int size, int align) => Guest.Alloc(size, align);

    [UnmanagedCallersOnly(EntryPoint = "wisp_handle")]
    public static long Handle(int ptr, int len) => Guest.Handle(ptr, len);

    [UnmanagedCallersOnly(EntryPoint = "wisp_wake")]
    public static long Wake(long future, int ptr, int len) => Guest.Wake(future, ptr, len);

    [UnmanagedCallersOnly(EntryPoint = "wisp_initialize")]
    public static void Initialize() => Guest.Initialize();

    [UnmanagedCallersOnly(EntryPoint = "wisp_gc_hint")]
    public static void GcHint(int pressure) => Guest.GcHint(pressure);
}
