using System;

namespace Isoplane.Abi;

internal static class RuntimeConfigurationInterop
{
    internal static unsafe byte[] Read()
    {
        int size = H.RuntimeConfigGet(null, 0);
        if (size < 0)
            throw new InvalidOperationException("The host refused runtime configuration.");
        byte[] bytes = new byte[size];
        fixed (byte* pointer = bytes)
        {
            if (H.RuntimeConfigGet(pointer, bytes.Length) != size)
                throw new InvalidOperationException("Runtime configuration changed while being read.");
        }
        return bytes;
    }
}
