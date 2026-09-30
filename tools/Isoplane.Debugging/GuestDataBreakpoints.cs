using System.Globalization;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Mono.Debugger.Soft;
using Mono.Debugging.Soft;
using Dap = Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;

namespace Isoplane.Debugging;

internal sealed class GuestDataBreakpoints(SoftDebuggerSession session, Func<int> allocate)
{
    private readonly Dictionary<string, ManagedDataBreakpoint> locations = [];
    private Dictionary<int, ActiveBreakpoint> active = [];
    private ManagedDataBreakpointEvents? pending;

    internal DataBreakpointInfoResponse Describe(PausedValues paused, DataBreakpointInfoArguments arguments)
    {
        try
        {
            var (context, expression) = paused.DataExpression(arguments);
            var target = ManagedLocations.Resolve(context, expression, allocate());
            string dataId = target.Id.ToString(CultureInfo.InvariantCulture);
            locations.Add(dataId, target);
            return new DataBreakpointInfoResponse
            {
                DataId = dataId, Description = expression, CanPersist = false,
                AccessTypes = [DataBreakpointAccessType.Write]
            };
        }
        catch (Exception error) when (error is ProtocolException or Mono.Debugging.Evaluation.EvaluatorException)
        {
            return new DataBreakpointInfoResponse { DataId = null, Description = error.Message, CanPersist = false };
        }
    }

    internal SetDataBreakpointsResponse Set(SetDataBreakpointsArguments arguments)
    {
        var next = new Dictionary<int, ActiveBreakpoint>();
        var response = new SetDataBreakpointsResponse();
        foreach (var requested in arguments.Breakpoints)
        {
            if (requested.AccessType is not (null or DataBreakpointAccessType.Write))
                throw new ProtocolException("Managed data breakpoints support value changes; read watchpoints are not supported.");
            if (!locations.TryGetValue(requested.DataId, out var target))
                throw new ProtocolException("The data breakpoint location is no longer available. Select it again while paused.");
            var hit = BreakpointHitCondition.Parse(requested.HitCondition);
            if (!next.TryAdd(target.Id, new ActiveBreakpoint(target, requested.Condition, hit)))
                throw new ProtocolException("The same data breakpoint was requested twice.");
            response.Breakpoints.Add(new Dap.Breakpoint { Id = target.Id, Verified = true });
        }
        if (session.IsConnected)
        {
            try { session.VirtualMachine.SetManagedDataBreakpoints(next.Values.Select(value => value.Target).ToArray()); }
            catch (Exception error) when (error is CommandException or ObjectCollectedException or VMNotSuspendedException or InvalidStackFrameException or NotSupportedException)
            {
                throw new ProtocolException($"The runtime rejected the managed data breakpoint locations ({error.Message}).", error);
            }
        }
        else if (next.Count != 0) throw new ProtocolException("Set data breakpoints while the guest is paused.");
        Volatile.Write(ref active, next);
        return response;
    }

    internal void Invalidate(Action<Dap.BreakpointEvent> changed)
    {
        foreach (int id in active.Keys)
            changed(new Dap.BreakpointEvent
            {
                Reason = Dap.BreakpointEvent.ReasonValue.Changed,
                Breakpoint = new Dap.Breakpoint { Id = id, Verified = false, Message = "The request ended; select this location again in the next guest." }
            });
        active.Clear();
        locations.Clear();
    }

    internal void Resuming()
    {
        foreach (var pair in locations.Where(pair => !active.ContainsKey(pair.Value.Id)).ToArray()) locations.Remove(pair.Key);
    }

    internal bool IgnoreExpirationStop(Action<Dap.BreakpointEvent> changed)
    {
        if (Volatile.Read(ref active).Count == 0) return false;
        var events = session.VirtualMachine.TakeManagedDataBreakpointHits();
        Expire(events, changed);
        if (events.Expired.Length != 0 && events.Hits.Length == 0) return true;
        pending = events with { Expired = [] };
        return false;
    }

    private void Expire(ManagedDataBreakpointEvents events, Action<Dap.BreakpointEvent> changed)
    {
        foreach (int id in events.Expired)
        {
            active.Remove(id);
            locations.Remove(id.ToString(CultureInfo.InvariantCulture));
            changed(new Dap.BreakpointEvent
            {
                Reason = Dap.BreakpointEvent.ReasonValue.Changed,
                Breakpoint = new Dap.Breakpoint { Id = id, Verified = false, Message = "The local left its scope or its invocation ended; select a live local again." }
            });
        }
    }

    internal DataStop TakeHits(PausedValues paused, Action<Dap.BreakpointEvent> changed)
    {
        var configured = Volatile.Read(ref active);
        if (configured.Count == 0) return new(false, [], null);
        var hits = pending ?? session.VirtualMachine.TakeManagedDataBreakpointHits();
        pending = null;
        var matched = new List<int>();
        string? description = null;
        Expire(hits, changed);
        foreach (int id in hits.Hits)
        {
            // An editor can remove a breakpoint while its stop event is in transit.
            if (!configured.TryGetValue(id, out var breakpoint)) continue;
            if (!breakpoint.HitCondition.Matches(++breakpoint.Hits)) continue;
            try
            {
                if (string.IsNullOrWhiteSpace(breakpoint.Condition) || paused.IsConditionMet(breakpoint.Condition)) matched.Add(id);
            }
            catch (ProtocolException error)
            {
                matched.Add(id);
                description = $"Data breakpoint {id} condition failed: {error.Message}";
            }
        }
        return new(hits.Hits.Length != 0 || hits.Expired.Length != 0, matched.ToArray(), description);
    }

    internal sealed record DataStop(bool IsDataStop, int[] BreakpointIds, string? Description);

    private sealed class ActiveBreakpoint(ManagedDataBreakpoint target, string? condition, BreakpointHitCondition hitCondition)
    {
        internal ManagedDataBreakpoint Target { get; } = target;
        internal string? Condition { get; } = condition;
        internal BreakpointHitCondition HitCondition { get; } = hitCondition;
        internal long Hits { get; set; }
    }
}
