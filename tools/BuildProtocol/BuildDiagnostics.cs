namespace Isoplane.Build;

/// <summary>A diagnostic the build reports, with the title the error code reference gives it.</summary>
internal sealed record BuildDiagnostic(string Code, string Title, bool IsError);

/// <summary>
/// The diagnostics only the build reports, from the assemblies it inspects. The SDK's analyzers declare
/// the others as descriptors; the error code reference is generated from both.
/// </summary>
internal static class BuildDiagnostics
{
    internal static readonly BuildDiagnostic UnreadableAssembly = new("ISOPLANE1000", "An assembly could not be read", IsError: true);
    internal static readonly BuildDiagnostic NativeImport = new("ISOPLANE1001", "An assembly declares a native import", IsError: false);
    internal static readonly BuildDiagnostic NoUsableEntry = new("ISOPLANE1005", "The application has no usable entry", IsError: true);

    internal static readonly BuildDiagnostic[] All = [UnreadableAssembly, NativeImport, NoUsableEntry];
}
