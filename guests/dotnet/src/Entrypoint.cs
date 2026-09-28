
#if ISOPLANE_MEMORY_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.MemoryProbe>>;
#elif ISOPLANE_LOG_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.LogProbe>>;
#elif ISOPLANE_STREAMING_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.StreamingProbe>>;
#elif ISOPLANE_ACTIVATION_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.ActivationProbe>>;
#elif ISOPLANE_CERTIFICATE_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.CertificateProbe>>;
#elif ISOPLANE_HASHING_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.HashingProbe>>;
#elif ISOPLANE_TRUST_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.TrustProbe>>;
#elif ISOPLANE_RANDOMNESS_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.RandomnessProbe>>;
#elif ISOPLANE_CONFIG_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.ConfigProbe>>;
#elif ISOPLANE_FAILING_BOOT
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.FailingBoot>>;
#elif ISOPLANE_NETWORK_REFUSAL_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.NetworkRefusalProbe>>;
#elif ISOPLANE_DNS_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.DnsProbe>>;
#elif ISOPLANE_STREAM_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.StreamProbe>>;
#elif ISOPLANE_CONNECT_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.ConnectProbe>>;
#elif ISOPLANE_CLEANUP_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.CleanupProbe>>;
#elif ISOPLANE_TIMER_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.TimerProbe>>;
#elif ISOPLANE_ASYNC_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.AsyncProbe>>;
#elif ISOPLANE_FILESYSTEM_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.FilesystemProbe>>;
#elif ISOPLANE_REQUEST_PROBE
using Guest = Isoplane.Abi.Exports<Isoplane.App.RequestProbeRouter>;
#else
using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Isoplane.App.Api>>;
#endif

/// <summary>Managed entry points invoked by the platform Mono entry layer.</summary>
internal static class IsoplaneGeneratedExports
{
    [System.Diagnostics.CodeAnalysis.DynamicDependency(
        System.Diagnostics.CodeAnalysis.DynamicallyAccessedMemberTypes.PublicMethods,
        typeof(IsoplaneGeneratedExports))]
    public static void Main() { }

    public static int Alloc(int size, int align) => Guest.Alloc(size, align);

    public static long Handle(int ptr, int len) => Guest.Handle(ptr, len);

    public static long Wake(long future, int ptr, int len) => Guest.Wake(future, ptr, len);

    public static long Initialize() => Guest.Initialize();

    public static long InitializeRuntime() => Guest.InitializeRuntime();

    public static void GcHint(int pressure) => Guest.GcHint(pressure);
}
