using System;

namespace Isoplane.Abi;

internal static class Initialization
{
    internal static unsafe byte[] ReadConfiguration()
    {
        int size = H.InitConfigGet(null, 0);
        if (size < 0)
            throw new InvalidOperationException("The host refused deployment configuration.");
        if (size == 0)
            return Array.Empty<byte>();
        byte[] data = new byte[size];
        fixed (byte* pointer = data)
        {
            if (H.InitConfigGet(pointer, data.Length) != size)
                throw new InvalidOperationException("Deployment configuration changed during initialization.");
        }
        return data;
    }
}
