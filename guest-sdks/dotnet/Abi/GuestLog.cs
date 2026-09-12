using System.Text;

namespace Wisp.Abi;

internal static unsafe class GuestLog
{
    internal static void Emit(int level, string fields)
    {
        byte[] bytes = Encoding.UTF8.GetBytes(fields);
        fixed (byte* pointer = bytes)
            H.LogEmit(level, pointer, bytes.Length);
    }
}
