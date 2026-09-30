using System;
using System.Linq;
using Mono.Debugger.Soft;
using Mono.Debugging.Client;

namespace Mono.Debugging.Soft;

public partial class SoftDebuggerSession
{
    /// <summary>Consumes protocol bookkeeping stops before they cancel an outstanding step.</summary>
    public Func<TargetEventArgs, bool> IgnoreInternalUserBreak { get; set; }

    private bool ResumeInternalUserBreak(Event[] events)
    {
        if (events.Any(value => value.EventType != EventType.UserBreak) || IgnoreInternalUserBreak is null)
            return false;
        var process = OnGetProcesses()[0];
        var thread = GetThread(process, events[0].Thread);
        var stopped = new TargetEventArgs(TargetEventType.TargetStopped)
        {
            Process = process, Thread = thread,
            Backtrace = GetBacktrace(process.Id, thread.Id)
        };
        if (!IgnoreInternalUserBreak(stopped)) return false;
        current_thread = null;
        vm.Resume();
        DequeueEventsForFirstThread();
        return true;
    }
}
