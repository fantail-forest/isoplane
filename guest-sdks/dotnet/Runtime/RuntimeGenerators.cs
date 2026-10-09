using System;
using System.Runtime.CompilerServices;

namespace Isoplane.Runtime;

/// <summary>
/// The generators that seeded themselves before the template snapshot, among
/// them <c>Random.Shared</c> and the one behind activity trace and span
/// identifiers. The snapshot copies their state, so every instance would
/// replay the template's sequence until they are reseeded from its own host.
/// </summary>
internal static class RuntimeGenerators
{
    // Only a guest starts from a snapshot. Tooling and tests run the SDK on a
    // desktop runtime, which has neither the snapshot nor the runtime's hook.
    internal static void Reseed()
    {
        if (OperatingSystem.IsWasi()) ReseedAfterActivation(null!);
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "ReseedAfterIsoplaneActivation")]
    private static extern void ReseedAfterActivation(Random random);
}
