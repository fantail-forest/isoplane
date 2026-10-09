using System.Linq;
using Mono.Debugger.Soft;
using Mono.Debugging.Client;

namespace Mono.Debugging.Soft;

public partial class SoftDebuggerSession
{
    private (long Thread, MethodMirror Method, int Offset, int Depth)? redirectedLocation;

    /// <summary>Moves to a validated IL offset and refreshes the stopped stack and stepping location.</summary>
    public TargetEventArgs MoveExecution(int offset) => MoveExecution(offset, false);

    private TargetEventArgs MoveExecution(int offset, bool remap)
    {
        if (!CanSetNextStatement || IsRunning || ActiveThread is null)
            throw new System.NotSupportedException("The guest must be paused in a frame that supports moving execution.");
        var thread = GetThread(ActiveThread.Id);
        var frame = thread.GetFrames().First();
        var location = frame.Method.LocationAtILOffset(offset)
            ?? throw new System.ArgumentException("The method has no statement at the requested offset.", nameof(offset));
        if (remap) thread.RemapIP(location);
        else thread.SetIP(location);
        StackVersion++;
        RaiseStopEvent();
        var stopped = ExecutionPosition.Refresh(this);
        var frames = GetThread(stopped.Thread.Id).GetFrames();
        currentAddress = offset;
        currentStackDepth = frames.Length;
        redirectedLocation = (stopped.Thread.Id, frames[0].Method, offset, frames.Length);
        return stopped;
    }

    /// <summary>Rebinds source breakpoints and refreshes the stopped stack after compiler updates.</summary>
    public TargetEventArgs RefreshAfterHotReload(int? remappedOffset = null)
    {
        var thread = ActiveThread ?? throw new System.InvalidOperationException("No guest thread is selected.");
        GetThread(thread.Id).InvalidateDebugFrames();
        var sourceBreakpoints = breakpoints.Values.Distinct()
            .Where(info => info.BreakEvent is Breakpoint and not FunctionBreakpoint && info.Location is not null).ToArray();
        foreach (var info in sourceBreakpoints)
        {
            foreach (var request in info.Requests.Keys)
            {
                request.Disable();
                breakpoints.Remove(request);
            }
            RemoveQueuedBreakEvents(info.Requests);
            info.Requests.Clear();
        }
        foreach (var info in sourceBreakpoints)
        {
            var breakpoint = (Breakpoint)info.BreakEvent;
            bool insideRange = false;
            var location = FindLocationByMethod(info.Location.Method, breakpoint.FileName, breakpoint.Line, breakpoint.Column, ref insideRange);
            if (location is null)
            {
                info.SetStatus(BreakEventStatus.NotBound, "The edited source has no executable statement at this location.");
                lock (pending_bes) if (!pending_bes.Contains(info)) pending_bes.Add(info);
                continue;
            }
            info.Location = location;
            InsertBreakpoint(breakpoint, info);
            breakpoint.UpdatedByEnC = false;
        }
        TryResolvePendingBreakpoints();
        if (remappedOffset is { } offset) return MoveExecution(offset, true);
        StackVersion++;
        RaiseStopEvent();
        return ExecutionPosition.Refresh(this);
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
