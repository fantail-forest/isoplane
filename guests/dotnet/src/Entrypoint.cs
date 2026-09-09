using System.Runtime.InteropServices;

#if WISP_STREAMING_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.StreamingProbe>>;
#elif WISP_HASHING_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.HashingProbe>>;
#elif WISP_RANDOMNESS_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.RandomnessProbe>>;
#elif WISP_CONFIG_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.ConfigProbe>>;
#elif WISP_FAILING_BOOT
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.FailingBoot>>;
#elif WISP_NETWORK_REFUSAL_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.NetworkRefusalProbe>>;
#elif WISP_DNS_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.DnsProbe>>;
#elif WISP_STREAM_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.StreamProbe>>;
#elif WISP_CONNECT_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.ConnectProbe>>;
#elif WISP_TIMER_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.TimerProbe>>;
#elif WISP_ASYNC_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.AsyncProbe>>;
#elif WISP_FILESYSTEM_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.FilesystemProbe>>;
#elif WISP_REQUEST_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.RequestProbe>>;
#else
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.Api>>;
#endif

namespace Wisp;

/// <summary>
/// This module's five core-wasm exports, each delegating to the SDK's
/// generic implementation; the <c>using Guest = …</c> alias above is the
/// whole declaration. The methods exist because
/// <c>[UnmanagedCallersOnly]</c> cannot be applied to a method of a generic
/// class while NativeAOT-LLVM needs the attribute to emit a wasm export.
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
