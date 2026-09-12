using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Threading.Tasks;

namespace Acceptance;

internal static class SocketApm
{
    public static async Task<string> RunAsync(string[] path)
    {
        string mode = path[0];
        using var socket = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
        await socket.ConnectAsync(new IPEndPoint(IPAddress.Parse(path[1]), int.Parse(path[2]))).ConfigureAwait(false);
        using var stream = new NetworkStream(socket, ownsSocket: false);
        var received = new TaskCompletionSource<int>();
        var sent = new TaskCompletionSource<int>();
        object state = new object();
        int readCallbacks = 0, writeCallbacks = 0;
        bool vector = mode.StartsWith("vector", StringComparison.Ordinal);
        byte[] buffer = new byte[vector ? 2 : 1];
        var receiveBuffers = vector ? new List<ArraySegment<byte>>
        {
            new(buffer, 0, 0), new(buffer, 0, 1), new(buffer, 1, 1)
        } : null;
        int EndRead(IAsyncResult result)
        {
            if (mode == "stream") return stream.EndRead(result);
            if (!mode.EndsWith("error", StringComparison.Ordinal)) return socket.EndReceive(result);
            int count = socket.EndReceive(result, out SocketError error);
            if (error != SocketError.Success) throw new SocketException((int)error);
            return count;
        }
        void ReadCompleted(IAsyncResult result)
        {
            try
            {
                readCallbacks++;
                if (!ReferenceEquals(result.AsyncState, state)) throw new InvalidOperationException("read state lost");
                received.SetResult(EndRead(result));
            }
            catch (Exception error) { received.SetException(error); }
        }
        SocketError status = SocketError.Success;
        IAsyncResult pending = mode switch
        {
            "stream" => stream.BeginRead(buffer, 0, 1, ReadCompleted, state),
            "socket" => socket.BeginReceive(buffer, 0, 1, SocketFlags.None, ReadCompleted, state),
            "socket-error" => socket.BeginReceive(buffer, 0, 1, SocketFlags.None, out status, ReadCompleted, state)!,
            "vector" => socket.BeginReceive(receiveBuffers!, SocketFlags.None, ReadCompleted, state),
            "vector-error" => socket.BeginReceive(receiveBuffers!, SocketFlags.None, out status, ReadCompleted, state)!,
            _ => throw new ArgumentException("unknown APM operation")
        };
        if (status != SocketError.Success || pending == null) throw new InvalidOperationException("read registration failed");
        if (pending.IsCompleted) throw new InvalidOperationException("peer cannot reply before send");
        bool refused = false;
        try { EndRead(pending); }
        catch (PlatformNotSupportedException) { refused = true; }
        catch (IOException error) when (error.InnerException is PlatformNotSupportedException) { refused = true; }
        if (!refused) throw new InvalidOperationException("unfinished EndRead was accepted");
        void WriteCompleted(IAsyncResult result)
        {
            try
            {
                writeCallbacks++;
                if (!ReferenceEquals(result.AsyncState, state)) throw new InvalidOperationException("write state lost");
                int count;
                if (mode == "stream") { stream.EndWrite(result); count = 1; }
                else if (mode.EndsWith("error", StringComparison.Ordinal))
                {
                    count = socket.EndSend(result, out SocketError error);
                    if (error != SocketError.Success) throw new SocketException((int)error);
                }
                else count = socket.EndSend(result);
                sent.SetResult(count);
            }
            catch (Exception error) { sent.SetException(error); }
        }
        byte[] message = vector ? new byte[] { 42, 43 } : new byte[] { 42 };
        var sendBuffers = vector ? new List<ArraySegment<byte>>
        {
            new(message, 0, 0), new(message, 0, 1), new(message, 1, 1)
        } : null;
        IAsyncResult write = mode switch
        {
            "stream" => stream.BeginWrite(message, 0, 1, WriteCompleted, state),
            "socket" => socket.BeginSend(message, 0, 1, SocketFlags.None, WriteCompleted, state),
            "socket-error" => socket.BeginSend(message, 0, 1, SocketFlags.None, out status, WriteCompleted, state)!,
            "vector" => socket.BeginSend(sendBuffers!, SocketFlags.None, WriteCompleted, state),
            "vector-error" => socket.BeginSend(sendBuffers!, SocketFlags.None, out status, WriteCompleted, state)!,
            _ => throw new ArgumentException("unknown APM operation")
        };
        if (status != SocketError.Success || write == null) throw new InvalidOperationException("write registration failed");
        int written = await sent.Task.ConfigureAwait(false);
        if (written <= 0 || written > message.Length)
            throw new InvalidOperationException("APM send did not return a valid partial transfer");
        await stream.WriteAsync(message.AsMemory(written)).ConfigureAwait(false);
        int read = await received.Task.ConfigureAwait(false);
        if (read <= 0 || read > buffer.Length)
            throw new InvalidOperationException("APM receive did not return a valid partial transfer");
        await stream.ReadExactlyAsync(buffer.AsMemory(read)).ConfigureAwait(false);
        if (buffer[0] != 42 || (vector && buffer[1] != 43) || readCallbacks != 1 || writeCallbacks != 1)
            throw new InvalidOperationException("APM transfer or callback count differs");
        return "APM callbacks restored; unfinished End refused";
    }
}
