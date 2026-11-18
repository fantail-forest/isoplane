using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Net;
using System.Net.Http;
using System.Net.Sockets;
using System.Runtime.CompilerServices;

namespace Isoplane.Runtime;

/// <summary>
/// What template initialization opened and has not closed, each with where it
/// was opened. The host refuses to save a template that holds a connection and
/// can name only its kind; the runtime keeps the origins while it is recording.
/// </summary>
internal static class OpenConnections
{
    private static bool _recording;

    // Only a guest prepares a template. Tooling and tests run the SDK on a
    // desktop runtime, which has no recording to start.
    internal static void Record()
    {
        if (_recording || !OperatingSystem.IsWasi()) return;
        _recording = true;
        RecordSockets(null!);
        RecordHandlers(null!, true);
        RecordLookups(null!);
    }

    /// <summary>Ends the recording and describes each socket still open and each DNS lookup still in progress.</summary>
    internal static List<string> Take()
    {
        var open = new List<string>();
        if (!_recording) return open;
        _recording = false;
        RecordHandlers(null!, false);
        foreach ((Socket socket, StackTrace origin) in TakeSockets(null!))
        {
            string description = $"a {socket.ProtocolType} socket {(Remote(socket) is { } remote ? $"connected to {remote}" : "that is not connected")}";
            // A pool opens its sockets on a background task, so only its handler's origin is in application code.
            open.Add(HandlerOf(null!, socket) is { } handler
                ? $"{description}, in the connection pool of an HTTP handler created\n{Frames(handler)}"
                : $"{description}, opened\n{Frames(origin)}");
        }
        foreach ((string name, StackTrace origin) in TakeLookups(null!))
            open.Add($"a DNS lookup of {name} that has not finished, started\n{Frames(origin)}");
        return open;
    }

    private static EndPoint? Remote(Socket socket)
    {
        if (!socket.Connected) return null;
        // The runtime's sockets are dual-stack, which reports an IPv4 peer in its IPv6 form.
        return socket.RemoteEndPoint is IPEndPoint { Address.IsIPv4MappedToIPv6: true } mapped
            ? new IPEndPoint(mapped.Address.MapToIPv4(), mapped.Port)
            : socket.RemoteEndPoint;
    }

    // The SDK's own frames, from the lifecycle hook that called the application downward, are the same in every trace.
    private static string Frames(StackTrace trace)
    {
        string frames = trace.ToString();
        int plumbing = frames.Length;
        foreach (string caller in (ReadOnlySpan<string>)["   at Isoplane.LifecycleHook.", "   at Isoplane.Runtime.", "   at Isoplane.Abi."])
        {
            int index = frames.IndexOf(caller, StringComparison.Ordinal);
            if (index >= 0 && index < plumbing) plumbing = index;
        }
        return frames[..plumbing];
    }

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "RecordIsoplaneSocketOrigins")]
    private static extern void RecordSockets(Socket socket);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "TakeIsoplaneOpenSockets")]
    private static extern KeyValuePair<Socket, StackTrace>[] TakeSockets(Socket socket);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "RecordIsoplaneConnectionOwners")]
    private static extern void RecordHandlers(SocketsHttpHandler handler, bool recording);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "IsoplaneConnectionOwner")]
    private static extern StackTrace? HandlerOf(SocketsHttpHandler handler, Socket socket);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "RecordIsoplaneLookups")]
    private static extern void RecordLookups(IPHostEntry entry);

    [UnsafeAccessor(UnsafeAccessorKind.StaticMethod, Name = "TakeIsoplanePendingLookups")]
    private static extern KeyValuePair<string, StackTrace>[] TakeLookups(IPHostEntry entry);
}
