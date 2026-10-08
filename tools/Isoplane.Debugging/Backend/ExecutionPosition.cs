using System;

namespace Mono.Debugging.Client
{
    /// <summary>Refreshes the stopped stack after changing the active thread's next instruction.</summary>
    public static class ExecutionPosition
    {
        /// <summary>Returns a fresh stopped stack connected to the session for expression evaluation.</summary>
        public static TargetEventArgs Refresh(DebuggerSession session)
        {
            var thread = session.ActiveThread ?? throw new InvalidOperationException("No guest thread is selected.");
            return new TargetEventArgs(TargetEventType.TargetStopped)
            {
                Thread = thread, Backtrace = session.GetBacktrace(thread.ProcessId, thread.Id)
            };
        }
    }
}
