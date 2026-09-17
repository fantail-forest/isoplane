using System.Text.Json.Nodes;
using Wisp.Native.Interop;

namespace Wisp.Native;

internal static class ModuleValidator
{
    internal static unsafe JsonObject Validate(byte[] module, byte[] policy)
    {
        NativeRuntime.EnsureLoaded();
        fixed (byte* wasm = module)
        fixed (byte* settings = policy)
            return NativeRuntime.Read(NativeMethods.isoplane_validate(wasm, (nuint)module.Length, settings, (nuint)policy.Length));
    }
}
