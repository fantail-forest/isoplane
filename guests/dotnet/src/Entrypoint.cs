using System.Runtime.InteropServices;

#if WISP_STREAMING_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.StreamingProbe>>;
#elif WISP_ACTIVATION_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.ActivationProbe>>;
#elif WISP_CERTIFICATE_PROBE
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.CertificateProbe>>;
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
using Guest = Wisp.Abi.Exports<Wisp.App.RequestProbeRouter>;
#else
using Guest = Wisp.Abi.Exports<Wisp.Durable.HandlerRouter<Wisp.App.Api>>;
#endif

/// <summary>
/// This module's five core-wasm exports, each delegating to the SDK's
/// generic implementation; the <c>using Guest = …</c> alias above is the
/// whole declaration. Under NativeAOT the methods carry
/// <c>[UnmanagedCallersOnly]</c>, which cannot sit on a generic class, and
/// become the wasm exports. Under Mono the SDK's C entry looks this class up
/// by name from the global namespace and invokes the same methods through the
/// runtime, so the class keeps the name the entry expects and a <c>Main</c>
/// that roots the methods through trimming.
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
    public static void Initialize() => Guest.Initialize();

#if !WISP_MONO
    [UnmanagedCallersOnly(EntryPoint = "wisp_gc_hint")]
#endif
    public static void GcHint(int pressure) => Guest.GcHint(pressure);
}
