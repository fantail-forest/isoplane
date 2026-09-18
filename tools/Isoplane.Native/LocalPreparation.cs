using System.Text.Json.Nodes;
using Isoplane.Native.Interop;

namespace Isoplane.Native;

internal static class LocalPreparation
{
    internal static unsafe JsonObject Prepare(byte[] module, byte[] options)
    {
        NativeRuntime.EnsureLoaded();
        fixed (byte* wasm = module)
        fixed (byte* settings = options)
            return NativeRuntime.Read(NativeMethods.isoplane_prepare_local(wasm, (nuint)module.Length,
                settings, (nuint)options.Length));
    }
}
