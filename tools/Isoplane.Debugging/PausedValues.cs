using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Mono.Debugging.Client;
using Dap = Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;

namespace Isoplane.Debugging;

internal sealed class PausedValues(TargetEventArgs stopped, Func<int> allocate, Mono.Debugging.Soft.SoftDebuggerSession session)
{
    private readonly Dictionary<int, Frame> frames = [];
    private readonly Dictionary<int, VariableSet> variables = [];
    internal int ThreadId => checked((int)stopped.Thread.Id);

    private List<Frame>? visibleFrames;

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
                ? new SourceLocation(suspended.Method.Name + " [awaiting]", suspended.Location.Source, suspended.Location.Line, suspended.Location.Column, suspended.Location.Line, suspended.Location.Column)
                : frame.Physical.SourceLocation;
            result.Add(new Dap.StackFrame
            {
                Id = id, Name = location.MethodName,
                Line = Math.Max(0, location.Line), Column = Math.Max(0, location.Column),
                Source = string.IsNullOrEmpty(location.FileName) ? null : new Source { Path = location.FileName }
            });
        }
        return new StackTraceResponse { StackFrames = result, TotalFrames = trace.Count };
    }

    private List<Frame> ReadFrames()
    {
        var code = new GuestCode(session.VirtualMachine);
        var physical = session.VirtualMachine.GetThreads().Single(thread => thread.Id == stopped.Thread.Id).GetFrames();
        var result = new List<Frame>();
        for (int index = 0; index < stopped.Backtrace.FrameCount; index++)
        {
            var frame = stopped.Backtrace.GetFrame(index);
            if (!frame.IsExternalCode && !frame.IsDebuggerHidden && physical[index].Method is { } method && code.Contains(method))
                result.Add(new Frame(frame, index));
            if (index == 0)
                result.AddRange(AsyncFrames.Read(session, code).Select(caller => new Frame(null, null, caller)));
        }
        return result;
    }

    internal ScopesResponse Scopes(int frameId)
    {
        if (!frames.TryGetValue(frameId, out var frame)) throw new ProtocolException("The stack frame is not part of the current pause.");
        int id = allocate();
        variables.Add(id, new VariableSet(frame.Locals, frame.Index, null));
        return new ScopesResponse { Scopes = [new Scope { Name = "Locals", VariablesReference = id, Expensive = false }] };
    }

    internal VariablesResponse Variables(int reference)
    {
        if (!variables.TryGetValue(reference, out var read)) throw new ProtocolException("The variable reference is not part of the current pause.");
        return new VariablesResponse { Variables = read.Read().Select(value => Variable(value, read.FrameIndex, read.Expression(value))).ToList() };
    }

    internal EvaluateResponse Evaluate(EvaluateArguments arguments)
    {
        var value = Evaluate(arguments.Expression, arguments.FrameId);
        var variable = Variable(value, FrameFor(arguments.FrameId).Index, arguments.Expression);
        return new EvaluateResponse { Result = variable.Value, Type = variable.Type, VariablesReference = variable.VariablesReference };
    }

    internal SetVariableResponse SetVariable(SetVariableArguments arguments)
    {
        if (!variables.TryGetValue(arguments.VariablesReference, out var read)) throw new ProtocolException("The variable reference is not part of the current pause.");
        if (read.FrameIndex is null) SavedLocalAssignment.RequireLiteral(arguments.Value);
        var matches = read.Read().Where(value => value.Name == arguments.Name).ToArray();
        if (matches.Length != 1) throw new ProtocolException("The named variable is missing or ambiguous in this scope.");
        Assign(matches[0], arguments.Value);
        var value = Variable(matches[0], read.FrameIndex, read.Expression(matches[0]));
        return new SetVariableResponse { Value = value.Value, Type = value.Type, VariablesReference = value.VariablesReference };
    }

    internal SetExpressionResponse SetExpression(SetExpressionArguments arguments)
    {
        var value = Evaluate(arguments.Expression, arguments.FrameId);
        Assign(value, arguments.Value);
        var variable = Variable(value, FrameFor(arguments.FrameId).Index, arguments.Expression);
        return new SetExpressionResponse { Value = variable.Value, Type = variable.Type, VariablesReference = variable.VariablesReference };
    }

    private ObjectValue Evaluate(string expression, int? frameId)
    {
        if (string.IsNullOrWhiteSpace(expression)) throw new ProtocolException("An expression is required.");
        var frame = FrameFor(frameId).Physical;
        var options = EvaluationOptions.DefaultOptions;
        var value = frame.GetExpressionValue(expression, options);
        if (value.IsEvaluating && !value.WaitHandle.WaitOne(options.EvaluationTimeout))
            throw new ProtocolException("Expression evaluation timed out.");
        if (value.IsError || value.IsUnknown || value.IsNotSupported || value.IsImplicitNotSupported)
            throw new ProtocolException(value.Value ?? "The expression cannot be evaluated in this frame.");
        return value;
    }

    internal bool IsConditionMet(string expression)
    {
        var value = Evaluate(expression, null).GetRawValue(EvaluationOptions.DefaultOptions);
        return value is bool result ? result : throw new ProtocolException("A data breakpoint condition must evaluate to bool.");
    }

    private static void Assign(ObjectValue value, string expression)
    {
        if (value.IsReadOnly) throw new ProtocolException("The selected value is read-only.");
        try { value.SetValue(expression, EvaluationOptions.DefaultOptions); }
        catch (Exception error) when (error is InvalidOperationException or NotSupportedException or Mono.Debugging.Evaluation.EvaluatorException)
        {
            throw new ProtocolException(error.Message, error);
        }
    }

    internal ExceptionInfoResponse ExceptionInfo(int threadId)
    {
        RequireThread(threadId);
        if (stopped.Type is not (TargetEventType.ExceptionThrown or TargetEventType.UnhandledException))
            throw new ProtocolException("The guest is not stopped on an exception.");
        var exception = stopped.Backtrace.GetFrame(0).GetException();
        if (exception is null) throw new ProtocolException("The runtime did not provide exception details.");
        return new ExceptionInfoResponse
        {
            ExceptionId = exception.Type, Description = exception.Message,
            BreakMode = stopped.Type == TargetEventType.UnhandledException ? ExceptionBreakMode.Unhandled : ExceptionBreakMode.Always,
            Details = new ExceptionDetails
            {
                TypeName = exception.Type, FullTypeName = exception.Type, Message = exception.Message,
                StackTrace = string.Join(Environment.NewLine, exception.StackTrace.Select(frame => frame.DisplayText))
            }
        };
    }

    private Variable Variable(ObjectValue value, int? frameIndex, string expression)
    {
        int reference = 0;
        if (value.HasChildren)
        {
            reference = allocate();
            variables.Add(reference, new VariableSet(value.GetAllChildren, frameIndex, expression));
        }
        return new Variable { Name = value.Name, Value = value.Value, Type = value.TypeName, VariablesReference = reference, EvaluateName = expression };
    }

    internal (int FrameIndex, string Expression) DataExpression(DataBreakpointInfoArguments arguments)
    {
        if (arguments.AsAddress == true || arguments.Bytes is not null)
            throw new ProtocolException("Data breakpoints support managed fields and array elements, not raw memory.");
        if (arguments.VariablesReference is not > 0) return (RequirePhysicalIndex(FrameFor(arguments.FrameId).Index), arguments.Name);
        if (!variables.TryGetValue(arguments.VariablesReference.Value, out var set))
            throw new ProtocolException("The variable reference is not part of the current pause.");
        var matches = set.Read().Where(value => value.Name == arguments.Name).ToArray();
        if (matches.Length != 1) throw new ProtocolException("The named variable is missing or ambiguous in this scope.");
        return (RequirePhysicalIndex(set.FrameIndex), set.Expression(matches[0]));
    }

    private static int RequirePhysicalIndex(int? index) => index
        ?? throw new ProtocolException("Data breakpoints cannot be configured from a suspended async caller.");

    private Frame FrameFor(int? frameId) => frameId is null ? new Frame(stopped.Backtrace.GetFrame(0), 0)
        : frames.TryGetValue(frameId.Value, out var known) ? known : throw new ProtocolException("The stack frame is not part of the current pause.");

    private sealed record Frame(Mono.Debugging.Client.StackFrame? Value, int? Index, AsyncFrame? Async = null)
    {
        internal Mono.Debugging.Client.StackFrame Physical => Value ?? throw new ProtocolException("Expression evaluation is not supported in a suspended async caller.");
        internal ObjectValue[] Locals() => Async is null ? Physical.GetAllLocals() : Async.Locals();
    }
    private sealed record VariableSet(Func<ObjectValue[]> Read, int? FrameIndex, string? ParentExpression)
    {
        internal string Expression(ObjectValue value) => ParentExpression is null ? value.Name : ParentExpression + value.ChildSelector;
    }

    internal void RequireThread(int threadId)
    {
        if (threadId != ThreadId) throw new ProtocolException("The requested thread is not the paused guest thread.");
    }
}
