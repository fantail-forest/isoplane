using System.Runtime.InteropServices;
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Handler>>;

internal static class Entrypoint
{
    /// <summary>Allocates guest request storage.</summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_alloc")]
    public static int Alloc(int size, int align) => Guest.Alloc(size, align);

    /// <summary>Dispatches a request through the SDK.</summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_handle")]
    public static long Handle(int ptr, int len) => Guest.Handle(ptr, len);

    /// <summary>Resumes a host completion.</summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_wake")]
    public static long Wake(long future, int ptr, int len) => Guest.Wake(future, ptr, len);

    /// <summary>Initializes the managed runtime.</summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_initialize")]
    public static void Initialize() => Guest.Initialize();

    /// <summary>Relays memory pressure to the SDK.</summary>
    [UnmanagedCallersOnly(EntryPoint = "wisp_gc_hint")]
    public static void GcHint(int pressure) => Guest.GcHint(pressure);
}
