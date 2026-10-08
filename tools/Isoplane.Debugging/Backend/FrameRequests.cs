using System;
using System.Runtime.ExceptionServices;

namespace Mono.Debugger.Soft;

public partial class Connection
{
    private void Send(CommandSet commandSet, int command, PacketWriter packet,
        Action<PacketReader> completed, int count, Action<Exception> failed)
    {
        try
        {
            Send(commandSet, command, packet, reply =>
            {
                try
                {
                    if (reply.ErrorCode != 0)
                        throw new CommandException((ErrorCode)reply.ErrorCode, reply.ErrorMsg);
                    completed(reply);
                }
                catch (Exception error) { failed(error); }
            }, count);
        }
        catch (Exception error)
        {
            failed(error);
        }
    }
}

public partial class ThreadMirror
{
    private ExceptionDispatchInfo frameFetchFailure;

    private void FetchFrameInfo(Action<FrameInfo[]> completed) =>
        vm.conn.Thread_GetFrameInfo(id, 0, -1, completed, error =>
        {
            lock (fetchingLocker)
            {
                frameFetchFailure = ExceptionDispatchInfo.Capture(error);
                fetching = false;
                cacheInvalid = true;
                fetchingEvent.Set();
            }
        });
}
