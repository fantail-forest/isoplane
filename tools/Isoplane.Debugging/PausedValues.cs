using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Mono.Debugging.Client;
using Dap = Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;

namespace Isoplane.Debugging;

internal sealed class PausedValues(TargetEventArgs stopped, Func<int> allocate, Mono.Debugging.Soft.SoftDebuggerSession session, SourcePaths sources) : IDisposable
{
    private readonly Dictionary<int, Frame> frames = [];
    private readonly Dictionary<int, VariableSet> variables = [];
    internal int ThreadId => checked((int)stopped.Thread.Id);

    private bool? boundaryFailure;
    internal bool IsUnhandled => stopped.Type == TargetEventType.UnhandledException || IsBoundaryFailure;
    private bool IsBoundaryFailure => boundaryFailure ??= IsGuestFailureBoundary();

    private bool IsGuestFailureBoundary()
    {
        var method = session.VirtualMachine.GetThreads().Single(thread => thread.Id == stopped.Thread.Id).GetFrames().FirstOrDefault()?.Method;
        return method?.Name == "NotifyUnhandled" && method.DeclaringType.FullName == "Isoplane.Abi.PollStatus"
            && method.DeclaringType.Assembly.GetName().Name == "Isoplane.Sdk";
    }

    private List<Frame>? visibleFrames;
    private StatementTargets? statements;

    internal GotoTargetsResponse GotoTargets(GotoTargetsArguments arguments) =>
        (IsUnhandled || stopped.Type == TargetEventType.ExceptionThrown)
            ? new GotoTargetsResponse { Targets = [] }
            : (statements ??= new StatementTargets(session, ThreadId, allocate, sources)).Find(arguments);

    internal TargetEventArgs Goto(GotoArguments arguments)
    {
        RequireThread(arguments.ThreadId);
        if (statements is null) throw new ProtocolException("Request a statement target before moving execution.");
        return statements.Move(arguments.TargetId);
    }

    internal StackTraceResponse StackTrace(StackTraceArguments arguments)
    {
        RequireThread(arguments.ThreadId);
        var trace = visibleFrames ??= ReadFrames();
        int start = arguments.StartFrame ?? 0;
        int count = arguments.Levels ?? 0;
        if (start < 0 || count < 0) throw new ProtocolException("Stack frame offset and count cannot be negative.");
        var result = new List<Dap.StackFrame>();
        int end = count == 0 ? trace.Count : (int)Math.Min(trace.Count, (long)start + count);
        for (int index = start; index < end; index++)
        {
            var frame = trace[index];
            int id = allocate();
            frames.Add(id, frame);
            var location = frame.Async is { } suspended
                ? suspended.Location is { } awaiting
                    ? new SourceLocation(suspended.Name + " [awaiting]", awaiting.Source, awaiting.Line, awaiting.Column, awaiting.Line, awaiting.Column)
                    : new SourceLocation(suspended.Name + " [awaiting]", null, 0, 0, 0, 0)
                : frame.Physical.SourceLocation;
            result.Add(new Dap.StackFrame
            {
                Id = id, Name = location.MethodName,
                Line = Math.Max(0, location.Line), Column = Math.Max(0, location.Column),
                Source = string.IsNullOrEmpty(location.FileName) ? null : new Source { Path = sources.ToClient(location.FileName) }
            });
        }
        return new StackTraceResponse { StackFrames = result, TotalFrames = trace.Count };
    }

    private List<Frame> ReadFrames()
    {
        var code = new GuestCode(session.VirtualMachine);
        var physical = session.VirtualMachine.GetThreads().Single(thread => thread.Id == stopped.Thread.Id).GetFrames();
        var result = new List<Frame>();
        foreach (int index in code.Shown(physical))
        {
            var frame = stopped.Backtrace.GetFrame(index);
            if (frame.IsDebuggerHidden) continue;
            result.Add(new Frame(frame, index));
            result.AddRange(AsyncFrames.Read(session, code, physical, index).Select(caller => new Frame(null, null, caller)));
        }
        return result;
    }

    internal ScopesResponse Scopes(int frameId)
    {
        if (!frames.TryGetValue(frameId, out var frame)) throw new ProtocolException("The stack frame is not part of the current pause.");
        int id = allocate();
        variables.Add(id, new VariableSet(frame.Locals, frame, null));
        return new ScopesResponse { Scopes = [new Scope { Name = "Locals", VariablesReference = id, Expensive = false }] };
    }

    internal VariablesResponse Variables(int reference)
    {
        if (!variables.TryGetValue(reference, out var read)) throw new ProtocolException("The variable reference is not part of the current pause.");
        return new VariablesResponse { Variables = read.Read().Select(value => Variable(value, read.Frame, read.Expression(value))).ToList() };
    }

    internal EvaluateResponse Evaluate(EvaluateArguments arguments)
    {
        var value = Evaluate(arguments.Expression, arguments.FrameId);
        var variable = Variable(value, FrameFor(arguments.FrameId), arguments.Expression);
        return new EvaluateResponse { Result = variable.Value, Type = variable.Type, VariablesReference = variable.VariablesReference };
    }

    internal SetVariableResponse SetVariable(SetVariableArguments arguments)
    {
        if (!variables.TryGetValue(arguments.VariablesReference, out var read)) throw new ProtocolException("The variable reference is not part of the current pause.");
        var matches = read.Read().Where(value => value.Name == arguments.Name).ToArray();
        if (matches.Length != 1) throw new ProtocolException("The named variable is missing or ambiguous in this scope.");
        string expression = read.Expression(matches[0]);
        var assigned = Assign(read.Frame, expression, arguments.Value);
        var value = Variable(assigned, read.Frame, expression);
        return new SetVariableResponse { Value = value.Value, Type = value.Type, VariablesReference = value.VariablesReference };
    }

    internal SetExpressionResponse SetExpression(SetExpressionArguments arguments)
    {
        var frame = FrameFor(arguments.FrameId);
        var value = Assign(frame, arguments.Expression, arguments.Value);
        var variable = Variable(value, frame, arguments.Expression);
        return new SetExpressionResponse { Value = variable.Value, Type = variable.Type, VariablesReference = variable.VariablesReference };
    }

    private ObjectValue Evaluate(string expression, int? frameId)
    {
        if (string.IsNullOrWhiteSpace(expression)) throw new ProtocolException("An expression is required.");
        var frame = FrameFor(frameId);
        var options = EvaluationOptions.DefaultOptions;
        ObjectValue value;
        try { value = frame.Async is { } saved ? saved.Evaluate(expression) : frame.Physical.GetExpressionValue(expression, options); }
        catch (Mono.Debugging.Evaluation.EvaluatorException error) { throw new ProtocolException(error.Message, error); }
        WaitForValue(value);
        if (value.IsError || value.IsUnknown || value.IsNotSupported || value.IsImplicitNotSupported)
            throw new ProtocolException(value.Value ?? "The expression cannot be evaluated in this frame.");
        return value;
    }

    private void WaitForValue(ObjectValue value)
    {
        FrameValues.Attach(stopped.Backtrace.GetFrame(0), value);
        if (value.IsEvaluating && !value.WaitHandle.WaitOne(EvaluationOptions.DefaultOptions.EvaluationTimeout))
            throw new ProtocolException("Value evaluation timed out.");
    }

    internal bool IsConditionMet(string expression)
    {
        var value = Evaluate(expression, null).GetRawValue(EvaluationOptions.DefaultOptions);
        return value is bool result ? result : throw new ProtocolException("A data breakpoint condition must evaluate to bool.");
    }

    private ObjectValue Assign(Frame frame, string target, string expression)
    {
        var context = Context(frame);
        try
        {
            var reference = context.Evaluator.Evaluate(context, target);
            if ((reference.Flags & ObjectValueFlags.ReadOnly) != 0)
                throw new ProtocolException("The selected value is read-only.");
            var value = context.Evaluator.Evaluate(context, expression, reference.Type);
            var converted = context.Adapter.Convert(context, value.Value, reference.Type);
            reference.SetValue(context, converted);
            return reference.CreateObjectValue(false);
        }
        catch (Exception error) when (error is InvalidOperationException or NotSupportedException or Mono.Debugging.Evaluation.EvaluatorException)
        {
            throw new ProtocolException(error.Message, error);
        }
    }

    internal ExceptionInfoResponse ExceptionInfo(int threadId)
    {
        RequireThread(threadId);
        if (!IsUnhandled && stopped.Type != TargetEventType.ExceptionThrown)
            throw new ProtocolException("The guest is not stopped on an exception.");
        var exception = ReadException();
        if (exception is null) throw new ProtocolException("The runtime did not provide exception details.");
        return new ExceptionInfoResponse
        {
            ExceptionId = exception.Type, Description = exception.Message,
            BreakMode = IsUnhandled ? ExceptionBreakMode.Unhandled : ExceptionBreakMode.Always,
            Details = new ExceptionDetails
            {
                TypeName = exception.Type, FullTypeName = exception.Type, Message = exception.Message,
                StackTrace = string.Join(Environment.NewLine, exception.StackTrace.Select(frame => frame.DisplayText))
            }
        };
    }

    private Mono.Debugging.Client.ExceptionInfo ReadException()
    {
        var context = Context(new Frame(stopped.Backtrace.GetFrame(0), 0));
        var error = IsBoundaryFailure ? context.Evaluator.Evaluate(context, "error") : context.Adapter.GetCurrentException(context);
        if (error is null) throw new ProtocolException("The runtime did not provide exception details.");
        var value = new Mono.Debugging.Evaluation.ExceptionInfoSource(context, error).CreateObjectValue(false, EvaluationOptions.DefaultOptions);
        WaitForValue(value);
        foreach (string name in new[] { "Message", "StackTrace" })
            if (value.GetChild(name) is { } child) WaitForValue(child);
        return new Mono.Debugging.Client.ExceptionInfo(value);
    }

    private Variable Variable(ObjectValue value, Frame frame, string expression)
    {
        WaitForValue(value);
        int reference = 0;
        if (value.HasChildren)
        {
            reference = allocate();
            variables.Add(reference, new VariableSet(() => value.GetAllChildren(EvaluationOptions.DefaultOptions), frame, expression));
        }
        return new Variable { Name = value.Name, Value = value.Value, Type = value.TypeName, VariablesReference = reference, EvaluateName = expression };
    }

    internal (Mono.Debugging.Soft.SoftEvaluationContext Context, string Expression) DataExpression(DataBreakpointInfoArguments arguments)
    {
        if (arguments.AsAddress == true || arguments.Bytes is not null)
            throw new ProtocolException("Data breakpoints support locals, managed fields and array elements; raw memory is unsupported.");
        if (arguments.VariablesReference is not > 0) return (Context(FrameFor(arguments.FrameId)), arguments.Name);
        if (!variables.TryGetValue(arguments.VariablesReference.Value, out var set))
            throw new ProtocolException("The variable reference is not part of the current pause.");
        var matches = set.Read().Where(value => value.Name == arguments.Name).ToArray();
        if (matches.Length != 1) throw new ProtocolException("The named variable is missing or ambiguous in this scope.");
        return (Context(set.Frame), set.Expression(matches[0]));
    }

    private Mono.Debugging.Soft.SoftEvaluationContext Context(Frame frame) => frame.Async?.Context
        ?? ManagedLocations.Context(session, frame.Index ?? throw new ProtocolException("The frame has no evaluation context."));

    /// <summary>Releases evaluators owned by the reconstructed callers at the end of this pause.</summary>
    public void Dispose()
    {
        if (visibleFrames is null) return;
        foreach (var frame in visibleFrames)
            frame.Async?.Context?.Adapter.Dispose();
    }

    private Frame FrameFor(int? frameId) => frameId is null ? new Frame(stopped.Backtrace.GetFrame(0), 0)
        : frames.TryGetValue(frameId.Value, out var known) ? known : throw new ProtocolException("The stack frame is not part of the current pause.");

    private sealed record Frame(Mono.Debugging.Client.StackFrame? Value, int? Index, AsyncFrame? Async = null)
    {
        internal Mono.Debugging.Client.StackFrame Physical => Value ?? throw new ProtocolException("The selected frame has no live invocation.");
        internal ObjectValue[] Locals() => Async is null ? Physical.GetAllLocals() : Async.Locals();
    }
    private sealed record VariableSet(Func<ObjectValue[]> Read, Frame Frame, string? ParentExpression)
    {
        internal string Expression(ObjectValue value) => ParentExpression is null ? value.Name : ParentExpression + value.ChildSelector;
    }

    internal void RequireThread(int threadId)
    {
        if (threadId != ThreadId) throw new ProtocolException("The requested thread is not the paused guest thread.");
    }
}
