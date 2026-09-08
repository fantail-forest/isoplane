using System;
using System.Net;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text;

namespace Wisp.Abi;

internal static unsafe class NativeDns
{
    internal static (int Stream, int Pollable) Start(string name)
    {
        byte[] bytes = Encoding.UTF8.GetBytes(name);
        int stream, pollable;
        fixed (byte* pointer = bytes)
        {
            int error = Start(pointer, bytes.Length, &stream, &pollable);
            if (error != 0) throw Failure(error - 1);
        }
        return (stream, pollable);
    }

    internal static IPAddress? Next(int stream)
    {
        byte* bytes = stackalloc byte[16];
        int length = Next(stream, bytes);
        if (length < 0) throw Failure(-length - 1);
        return length == 0 ? null : new IPAddress(new ReadOnlySpan<byte>(bytes, length));
    }

    private static SocketException Failure(int error) => new((int)((NetworkErrorCode)error switch
    {
        NetworkErrorCode.AccessDenied => SocketError.AccessDenied,
        NetworkErrorCode.NotSupported => SocketError.OperationNotSupported,
        NetworkErrorCode.InvalidArgument => SocketError.InvalidArgument,
        NetworkErrorCode.OutOfMemory or NetworkErrorCode.NewSocketLimit => SocketError.NoBufferSpaceAvailable,
        NetworkErrorCode.Timeout => SocketError.TimedOut,
        NetworkErrorCode.NameUnresolvable => SocketError.HostNotFound,
        NetworkErrorCode.TemporaryResolverFailure => SocketError.TryAgain,
        NetworkErrorCode.PermanentResolverFailure => SocketError.NoRecovery,
        _ => SocketError.SocketError,
    }));

    [DllImport("Wisp.Native", EntryPoint = "wisp_dns_start")]
    private static extern int Start(byte* name, int length, int* stream, int* pollable);

    [DllImport("Wisp.Native", EntryPoint = "wisp_dns_next")]
    private static extern int Next(int stream, byte* address);

    [DllImport("Wisp.Native", EntryPoint = "wisp_dns_drop")]
    internal static extern void Drop(int stream);
}
