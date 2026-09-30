using System;

namespace Mono.Debugging.Client
{
    /// <summary>Refreshes the stopped stack after changing the active thread's next instruction.</summary>
    public static class ExecutionPosition
    {
        /// <summary>Moves to a validated IL offset and returns the newly stopped thread and stack.</summary>
        public static TargetEventArgs Move(DebuggerSession session, int offset)
        {
            var thread = session.ActiveThread ?? throw new InvalidOperationException("No guest thread is selected.");
            session.SetNextStatement(offset);
            return new TargetEventArgs(TargetEventType.TargetStopped)
            {
                Thread = thread, Backtrace = session.GetBacktrace(thread.ProcessId, thread.Id)
            };
        }
    }
}
