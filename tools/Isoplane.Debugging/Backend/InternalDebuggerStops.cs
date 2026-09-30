using System;
using System.Linq;
using Mono.Debugger.Soft;

namespace Mono.Debugging.Soft;

public partial class SoftDebuggerSession
{
    /// <summary>Consumes protocol bookkeeping stops before they cancel an outstanding step.</summary>
    public Func<bool> IgnoreInternalUserBreak { get; set; }

    private bool ResumeInternalUserBreak(Event[] events)
    {
        if (events.Any(value => value.EventType != EventType.UserBreak) || IgnoreInternalUserBreak?.Invoke() != true)
            return false;
        current_thread = null;
        vm.Resume();
        DequeueEventsForFirstThread();
        return true;
    }
}
