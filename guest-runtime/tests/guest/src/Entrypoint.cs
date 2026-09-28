using Guest = Isoplane.Abi.Exports<Isoplane.Durable.HandlerRouter<Acceptance.AcceptanceHandler>>;

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
