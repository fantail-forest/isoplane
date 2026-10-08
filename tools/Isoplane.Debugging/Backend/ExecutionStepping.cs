using System.Linq;
using Mono.Debugger.Soft;
using Mono.Debugging.Client;

namespace Mono.Debugging.Soft;

public partial class SoftDebuggerSession
{
    private (long Thread, MethodMirror Method, int Offset, int Depth)? redirectedLocation;

    /// <summary>Moves to a validated IL offset and refreshes the stopped stack and stepping location.</summary>
    public TargetEventArgs MoveExecution(int offset)
    {
        var stopped = ExecutionPosition.Move(this, offset);
        var frames = GetThread(stopped.Thread.Id).GetFrames();
        currentAddress = offset;
        currentStackDepth = frames.Length;
        redirectedLocation = (stopped.Thread.Id, frames[0].Method, offset, frames.Length);
        return stopped;
    }

    private bool ResumeRedirectedBreakpoint(Event[] events)
    {
        var redirected = redirectedLocation;
        redirectedLocation = null;
        if (redirected is not { } location || currentRequest is not StepEventRequest step
            || events.Any(value => value.EventType != EventType.Breakpoint || value.Thread.Id != location.Thread))
            return false;
        var frames = events[0].Thread.GetFrames();
        if (frames.Length != location.Depth || frames[0].Method != location.Method || frames[0].ILOffset != location.Offset)
            return false;
        // The first step was armed before the interpreter resumed at the new IP.
        // Re-arm it here so its starting source line is the jump destination.
        foreach (var breakpoint in events) HandleBreakpoint(breakpoint.Thread, breakpoint.Request);
        step.Enabled = false;
        currentRequest = null;
        current_thread = recent_thread = events[0].Thread;
        Step(step.Depth, step.Size);
        return true;
    }
}
