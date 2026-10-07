using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Mono.Debugging.Client;
using Mono.Debugging.Soft;
using Dap = Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;

namespace Isoplane.Debugging;

internal sealed class GuestBreakpoints
{
    private SoftDebuggerSession session;
    private readonly Func<int> allocate;
    private readonly Action<BreakpointEvent> changed;
    private volatile ApplicationSymbols symbols = ApplicationSymbols.None;
    private readonly Dictionary<Mono.Debugging.Client.Breakpoint, int> identifiers = [];
    private readonly object gate = new();
    private volatile bool breakOnUnhandled = true;

    internal GuestBreakpoints(SoftDebuggerSession session, Func<int> allocate, Action<BreakpointEvent> changed, Action<OutputEvent> output)
    {
        this.session = session;
        this.allocate = allocate;
        this.changed = changed;
        session.Breakpoints.BreakpointStatusChanged += (_, args) =>
        {
            int id;
            lock (gate) if (!identifiers.TryGetValue(args.Breakpoint, out id)) return;
            Report(args.Breakpoint, id);
        };
        Bind(session, output);
    }

    internal void Bind(SoftDebuggerSession next, Action<OutputEvent> output)
    {
        session = next;
        session.BreakpointTraceHandler = (breakpoint, text) => output(new OutputEvent
        {
            Category = OutputEvent.CategoryValue.Console, Output = text + Environment.NewLine,
            Source = breakpoint is Mono.Debugging.Client.Breakpoint source && source.FileName is not null ? new Source { Path = source.FileName } : null
        });
    }

    /// <summary>Takes the symbols of the application as last built, and tells the editor what they change about its breakpoints.</summary>
    internal void Rebuilt(ApplicationSymbols application)
    {
        symbols = application;
        KeyValuePair<Mono.Debugging.Client.Breakpoint, int>[] current;
        lock (gate) current = [.. identifiers];
        foreach (var (breakpoint, id) in current) Report(breakpoint, id);
    }

    private void Report(Mono.Debugging.Client.Breakpoint breakpoint, int id) =>
        changed(new BreakpointEvent { Reason = BreakpointEvent.ReasonValue.Changed, Breakpoint = Describe(breakpoint, id) });

    internal SetBreakpointsResponse SetSource(SetBreakpointsArguments arguments)
    {
        string path = arguments.Source?.Path ?? throw new ProtocolException("A source path is required.");
        if (!Path.IsPathFullyQualified(path)) throw new ProtocolException("Breakpoint source paths must be absolute.");
        var requested = (arguments.Breakpoints ?? []).Select(item =>
        {
            if (item.Line <= 0 || item.Column is <= 0) throw new ProtocolException("Source breakpoints require positive line and column numbers.");
            // A column asks for one statement of the line. The debugger binds only where a statement starts,
            // so the caret's column becomes the start of the statement it is in.
            int column = item.Column is > 1 and var caret ? symbols.StatementAt(path, item.Line, caret)?.Column ?? caret : 1;
            var breakpoint = new Mono.Debugging.Client.Breakpoint(path, item.Line, column);
            Configure(breakpoint, item.Condition, item.HitCondition, item.LogMessage);
            return breakpoint;
        }).ToArray();
        Remove(session.Breakpoints.GetBreakpointsAtFile(path).Where(item => item is not Mono.Debugging.Client.FunctionBreakpoint).ToArray());
        return new SetBreakpointsResponse { Breakpoints = requested.Select(Add).ToList() };
    }

    /// <summary>Where breakpoints can be set on the requested lines, for the editor to offer the statements within a line.</summary>
    internal BreakpointLocationsResponse Locations(BreakpointLocationsArguments arguments)
    {
        string path = arguments.Source?.Path ?? throw new ProtocolException("A source path is required.");
        if (!Path.IsPathFullyQualified(path) || arguments.Line <= 0)
            throw new ProtocolException("Breakpoint locations require an absolute source path and a positive line.");
        return new BreakpointLocationsResponse
        {
            Breakpoints = [.. symbols.StatementsStarting(path, arguments.Line, arguments.EndLine ?? arguments.Line).Select(statement =>
                new BreakpointLocation { Line = statement.Line, Column = statement.Column, EndLine = statement.EndLine, EndColumn = statement.EndColumn })]
        };
    }

    internal SetFunctionBreakpointsResponse SetFunctions(SetFunctionBreakpointsArguments arguments)
    {
        var requested = arguments.Breakpoints.Select(item =>
        {
            if (string.IsNullOrWhiteSpace(item.Name)) throw new ProtocolException("A function breakpoint requires a qualified method name.");
            var breakpoint = new Mono.Debugging.Client.FunctionBreakpoint(item.Name, "C#");
            if (string.IsNullOrEmpty(breakpoint.TypeName) || string.IsNullOrEmpty(breakpoint.MethodName))
                throw new ProtocolException("Function breakpoints require a qualified Type.Method name.");
            Configure(breakpoint, item.Condition, item.HitCondition, null);
            return breakpoint;
        }).ToArray();
        Remove(session.Breakpoints.OfType<Mono.Debugging.Client.FunctionBreakpoint>().ToArray());
        return new SetFunctionBreakpointsResponse { Breakpoints = requested.Select(Add).ToList() };
    }

    internal SetExceptionBreakpointsResponse SetExceptions(SetExceptionBreakpointsArguments arguments)
    {
        if (arguments.Filters.Any(filter => filter is not ("all" or "uncaught")))
            throw new ProtocolException("Exception filters must be all or uncaught.");
        foreach (var old in session.Breakpoints.OfType<Catchpoint>().ToArray()) session.Breakpoints.Remove(old);
        if (arguments.Filters.Contains("all")) session.Breakpoints.Add(new Catchpoint("System.Exception"));
        breakOnUnhandled = arguments.Filters.Contains("uncaught") || arguments.Filters.Contains("all");
        return new SetExceptionBreakpointsResponse();
    }

    internal bool ShouldStop(bool unhandled) => !unhandled || breakOnUnhandled;

    private static void Configure(BreakEvent breakpoint, string? condition, string? hitCondition, string? logMessage)
    {
        breakpoint.ConditionExpression = condition;
        if (!string.IsNullOrWhiteSpace(hitCondition))
        {
            var hit = BreakpointHitCondition.Parse(hitCondition);
            breakpoint.HitCount = hit.Count;
            breakpoint.HitCountMode = hit.Mode;
        }
        if (logMessage is not null)
        {
            breakpoint.HitAction = HitAction.PrintExpression;
            breakpoint.TraceExpression = logMessage;
        }
    }

    private Dap.Breakpoint Add(Mono.Debugging.Client.Breakpoint breakpoint)
    {
        int id = allocate();
        lock (gate) identifiers.Add(breakpoint, id);
        session.Breakpoints.Add(breakpoint);
        return Describe(breakpoint, id);
    }

    private void Remove(IEnumerable<Mono.Debugging.Client.Breakpoint> breakpoints)
    {
        foreach (var breakpoint in breakpoints)
        {
            lock (gate) identifiers.Remove(breakpoint);
            session.Breakpoints.Remove(breakpoint);
        }
    }

    private Dap.Breakpoint Describe(Mono.Debugging.Client.Breakpoint breakpoint, int id)
    {
        // A guest binds a breakpoint when it loads the code, which is while a request runs. Between
        // requests the built application's symbols answer instead, so the editor shows at once
        // whether the breakpoint will be reached.
        bool? statement = breakpoint is Mono.Debugging.Client.FunctionBreakpoint || breakpoint.FileName is null
            ? null : symbols.HasStatement(breakpoint.FileName, breakpoint.Line, breakpoint.Column);
        bool verified = statement == true || breakpoint.GetStatus(session) == BreakEventStatus.Bound;
        return new()
        {
            Id = id, Verified = verified,
            Line = breakpoint.Line, Column = breakpoint.Column,
            Source = breakpoint.FileName is null ? null : new Source { Path = breakpoint.FileName },
            Message = verified ? null : statement == false ? "No statement of the built application starts on this line."
                : breakpoint.GetStatusMessage(session)
        };
    }
}
