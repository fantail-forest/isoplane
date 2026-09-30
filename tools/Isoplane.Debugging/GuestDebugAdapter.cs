using System.Net;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Mono.Debugging.Client;
using Mono.Debugging.Soft;
using Newtonsoft.Json.Linq;
using Dap = Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;

namespace Isoplane.Debugging;

internal sealed class GuestDebugAdapter : DebugAdapterBase, IAsyncDisposable
{
    private SoftDebuggerSession session = new() { Breakpoints = new BreakpointStore() };
    private readonly TextWriter diagnostics;
    private readonly GuestBreakpoints breakpoints;
    private GuestDataBreakpoints dataBreakpoints;
    private IPEndPoint? endpoint;
    private Task reconnecting = Task.CompletedTask;
    private readonly object lifecycle = new();
    private GuestConnection? connection;
    private GuestSession? owner;
    private Task opening = Task.CompletedTask;
    private IRequestResponder<AttachArguments>? attach;
    private PausedValues? paused;
    private int nextHandle;
    private int pauseRequested;
    private Mono.Debugger.Soft.StepEventRequest? pauseStep;
    private int stopping;
    private int ended;
    private int failed;
    private int disposed;
    private bool initialized;
    private bool configured;

    internal bool Failed => Volatile.Read(ref failed) != 0;

    internal GuestDebugAdapter(Stream input, Stream output, TextWriter diagnostics)
    {
        this.diagnostics = diagnostics;
        InitializeProtocolClient(input, output);
        ConfigureSession(session);
        dataBreakpoints = new GuestDataBreakpoints(session, Allocate);
        breakpoints = new GuestBreakpoints(session, Allocate, message => Protocol.SendEvent(message), message => Protocol.SendEvent(message));
    }

    private void ConfigureSession(SoftDebuggerSession backend)
    {
        backend.TargetHitBreakpoint += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Breakpoint);
        backend.TargetInterrupted += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Pause);
        backend.TargetStopped += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Step);
        backend.TargetExceptionThrown += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Exception);
        backend.TargetUnhandledException += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Exception);
        backend.TargetReady += (_, _) => Interlocked.Exchange(ref attach, null)?.SetResponse(new AttachResponse());
        backend.TargetExited += (_, _) => Ended();
        backend.ExceptionHandler = error => { if (!backend.HasExited) Fail(error); return true; };
        backend.LogWriter = (_, text) => diagnostics.WriteLine(text);
        backend.OutputWriter = (_, text) => diagnostics.WriteLine(text);
    }

    protected override void HandleInitializeRequestAsync(IRequestResponder<InitializeArguments, InitializeResponse> request)
    {
        var args = request.Arguments;
        if (initialized)
        {
            request.SetError(new ProtocolException("The debug adapter is already initialized."));
            return;
        }
        if (args.PathFormat is not null && args.PathFormat != InitializeArguments.PathFormatValue.Path
            || args.LinesStartAt1 == false || args.ColumnsStartAt1 == false)
        {
            request.SetError(new ProtocolException("The local debugger requires native paths and one-based source positions."));
            return;
        }
        initialized = true;
        request.SetResponse(new InitializeResponse
        {
            SupportsConfigurationDoneRequest = true, SupportTerminateDebuggee = true,
            SupportsConditionalBreakpoints = true, SupportsHitConditionalBreakpoints = true, SupportsLogPoints = true,
            SupportsFunctionBreakpoints = true, SupportsEvaluateForHovers = true, SupportsSetVariable = true,
            SupportsSetExpression = true, SupportsExceptionInfoRequest = true, SupportsDataBreakpoints = true,
            SupportsDataBreakpointBytes = false,
            ExceptionBreakpointFilters =
            [
                new ExceptionBreakpointsFilter { Filter = "all", Label = "All thrown exceptions" },
                new ExceptionBreakpointsFilter { Filter = "uncaught", Label = "Uncaught exceptions", Default = true }
            ]
        });
    }

    protected override void HandleAttachRequestAsync(IRequestResponder<AttachArguments> request)
    {
        if (!initialized)
        {
            request.SetError(new ProtocolException("Initialize the debug adapter before attaching."));
            return;
        }
        if (owner is not null)
        {
            request.SetError(new ProtocolException("This adapter already owns a debug session."));
            return;
        }
        var options = request.Arguments.ConfigurationProperties;
        string? address = options.TryGetValue("address", out var addressValue) && addressValue.Type == JTokenType.String ? addressValue.Value<string>() : null;
        long? port = options.TryGetValue("port", out var portValue) && portValue.Type == JTokenType.Integer ? portValue.Value<long>() : null;
        if (address != "127.0.0.1" || port is null or < 1 or > 65535)
        {
            request.SetError(new ProtocolException("Attach requires the session-owned address 127.0.0.1 and port 1..65535."));
            return;
        }
        owner = new GuestSession(SessionEnded);
        attach = request;
        opening = OpenAsync(new IPEndPoint(IPAddress.Loopback, (int)port.Value));
    }

    private async Task OpenAsync(IPEndPoint address)
    {
        try
        {
            var target = await owner!.ConnectAsync(address);
            lock (lifecycle)
            {
                if (Volatile.Read(ref stopping) != 0) return;
                endpoint = target;
                connection = new GuestConnection(target, Fail);
                Protocol.SendEvent(new InitializedEvent());
            }
        }
        catch (Exception error) { Fail(error); }
    }

    protected override void HandleConfigurationDoneRequestAsync(IRequestResponder<ConfigurationDoneArguments> request)
    {
        if (connection is null || configured)
        {
            request.SetError(new ProtocolException("Configure one attached session before starting the guest."));
            return;
        }
        configured = true;
        request.SetResponse(new ConfigurationDoneResponse());
        session.Run(new SoftDebuggerStartInfo(connection), new DebuggerSessionOptions { EvaluationOptions = EvaluationOptions.DefaultOptions });
    }

    protected override SetBreakpointsResponse HandleSetBreakpointsRequest(SetBreakpointsArguments arguments)
    {
        lock (lifecycle) return breakpoints.SetSource(arguments);
    }
    protected override SetFunctionBreakpointsResponse HandleSetFunctionBreakpointsRequest(SetFunctionBreakpointsArguments arguments)
    {
        lock (lifecycle) return breakpoints.SetFunctions(arguments);
    }
    protected override SetExceptionBreakpointsResponse HandleSetExceptionBreakpointsRequest(SetExceptionBreakpointsArguments arguments)
    {
        lock (lifecycle) return breakpoints.SetExceptions(arguments);
    }

    protected override DataBreakpointInfoResponse HandleDataBreakpointInfoRequest(DataBreakpointInfoArguments arguments) => dataBreakpoints.Describe(Pause, arguments);
    protected override SetDataBreakpointsResponse HandleSetDataBreakpointsRequest(SetDataBreakpointsArguments arguments) => dataBreakpoints.Set(arguments);

    protected override ThreadsResponse HandleThreadsRequest(ThreadsArguments arguments) => new()
    {
        Threads = session.GetProcesses().SelectMany(process => process.GetThreads())
            .Select(thread => new Dap.Thread { Id = checked((int)thread.Id), Name = thread.Name }).ToList()
    };

    private PausedValues Pause => Volatile.Read(ref paused) ?? throw new ProtocolException("The guest is not paused.");
    private int Allocate() => Interlocked.Increment(ref nextHandle);
    protected override StackTraceResponse HandleStackTraceRequest(StackTraceArguments arguments) => Pause.StackTrace(arguments);
    protected override ScopesResponse HandleScopesRequest(ScopesArguments arguments) => Pause.Scopes(arguments.FrameId);
    protected override VariablesResponse HandleVariablesRequest(VariablesArguments arguments) => Pause.Variables(arguments.VariablesReference);

    protected override EvaluateResponse HandleEvaluateRequest(EvaluateArguments arguments) => Pause.Evaluate(arguments);
    protected override SetVariableResponse HandleSetVariableRequest(SetVariableArguments arguments) => Pause.SetVariable(arguments);
    protected override SetExpressionResponse HandleSetExpressionRequest(SetExpressionArguments arguments) => Pause.SetExpression(arguments);
    protected override ExceptionInfoResponse HandleExceptionInfoRequest(ExceptionInfoArguments arguments) => Pause.ExceptionInfo(arguments.ThreadId);

    protected override StepInResponse HandleStepInRequest(StepInArguments arguments)
    {
        Resume(arguments.ThreadId, session.StepLine);
        return new StepInResponse();
    }

    protected override StepOutResponse HandleStepOutRequest(StepOutArguments arguments)
    {
        Resume(arguments.ThreadId, session.Finish);
        return new StepOutResponse();
    }

    protected override PauseResponse HandlePauseRequest(PauseArguments arguments)
    {
        if (!session.IsConnected || !session.IsRunning) throw new ProtocolException("Pause requires a running guest.");
        if (!session.GetProcesses().SelectMany(process => process.GetThreads()).Any(thread => thread.Id == arguments.ThreadId))
            throw new ProtocolException("The requested thread is not a running guest thread.");
        if (Interlocked.CompareExchange(ref pauseRequested, 1, 0) != 0) throw new ProtocolException("A pause request is already pending.");
        try
        {
            var machine = session.VirtualMachine;
            machine.Suspend();
            try
            {
                var threads = machine.GetThreads();
                if (threads.Count != 1) throw new ProtocolException("The local WASM debugger requires one guest thread.");
                // Deliver pause through the backend's event loop, which owns the stopped thread.
                var step = machine.CreateStepRequest(threads[0]);
                step.Depth = Mono.Debugger.Soft.StepDepth.Into;
                step.Size = Mono.Debugger.Soft.StepSize.Min;
                step.Count = 1;
                Volatile.Write(ref pauseStep, step);
                step.Enable();
            }
            finally { machine.Resume(); }
        }
        catch
        {
            Interlocked.Exchange(ref pauseRequested, 0);
            throw;
        }
        return new PauseResponse();
    }

    protected override NextResponse HandleNextRequest(NextArguments arguments)
    {
        Resume(arguments.ThreadId, session.NextLine);
        return new NextResponse();
    }

    protected override ContinueResponse HandleContinueRequest(ContinueArguments arguments)
    {
        Resume(arguments.ThreadId, session.Continue);
        return new ContinueResponse { AllThreadsContinued = true };
    }

    private void Resume(int threadId, Action resume)
    {
        Pause.RequireThread(threadId);
        Interlocked.Exchange(ref paused, null)?.Dispose();
        dataBreakpoints.Resuming();
        resume();
    }

    private void Stopped(TargetEventArgs args, StoppedEvent.ReasonValue reason)
    {
        if (Volatile.Read(ref stopping) != 0) return;
        if (!breakpoints.ShouldStop(args))
        {
            session.Continue();
            return;
        }
        if (args.Backtrace is null || args.Thread is null)
        {
            Fail(new InvalidOperationException("The debugger stopped without a guest thread and stack trace."));
            return;
        }
        if (Interlocked.Exchange(ref pauseRequested, 0) != 0)
        {
            var step = Interlocked.Exchange(ref pauseStep, null);
            if (step?.Enabled == true) step.Disable();
            reason = StoppedEvent.ReasonValue.Pause;
        }
        var state = new PausedValues(args, Allocate, session);
        var hits = dataBreakpoints.TakeHits(state);
        if (hits.IsDataStop)
        {
            if (hits.BreakpointIds.Length == 0)
            {
                state.Dispose();
                session.Continue();
                return;
            }
            reason = StoppedEvent.ReasonValue.DataBreakpoint;
        }
        Volatile.Write(ref paused, state);
        Protocol.SendEvent(new StoppedEvent { Reason = reason, ThreadId = state.ThreadId, AllThreadsStopped = true, HitBreakpointIds = hits.BreakpointIds.ToList(), Description = hits.Description });
    }

    protected override void HandleDisconnectRequestAsync(IRequestResponder<DisconnectArguments> request)
    {
        if (request.Arguments.TerminateDebuggee == false && Volatile.Read(ref ended) == 0)
        {
            request.SetError(new ProtocolException("Detaching while leaving the guest running is not supported. Stop the local session or set terminateDebuggee to true."));
            return;
        }
        request.SetResponse(new DisconnectResponse());
        Stop();
    }

    protected override void HandleProtocolError(Exception error) => Fail(error);

    private void Fail(Exception error)
    {
        if (Volatile.Read(ref stopping) != 0 || Volatile.Read(ref ended) != 0) return;
        Interlocked.Exchange(ref failed, 1);
        diagnostics.WriteLine($"isoplane debugger: {error}");
        Interlocked.Exchange(ref attach, null)?.SetError(new ProtocolException(error.Message));
        Stop();
    }

    private void SessionEnded()
    {
        lock (lifecycle)
        {
            if (Interlocked.Exchange(ref ended, 1) != 0 || Volatile.Read(ref stopping) != 0) return;
            Interlocked.Exchange(ref paused, null)?.Dispose();
            Interlocked.Exchange(ref attach, null)?.SetError(new ProtocolException("The local development session ended."));
            connection?.Close();
            Protocol.SendEvent(new TerminatedEvent());
        }
    }

    private void Ended()
    {
        lock (lifecycle)
        {
            Interlocked.Exchange(ref paused, null)?.Dispose();
            if (Volatile.Read(ref stopping) != 0 || Volatile.Read(ref ended) != 0) return;
            if (attach is not null)
            {
                Fail(new InvalidOperationException("The guest exited before attachment completed."));
                return;
            }
            reconnecting = ReconnectAsync(session, connection!);
        }
    }

    private async Task ReconnectAsync(SoftDebuggerSession previous, GuestConnection previousConnection)
    {
        try
        {
            await Task.Yield();
            await previousConnection.DisposeAsync();
            lock (lifecycle)
            {
                if (Volatile.Read(ref stopping) != 0 || Volatile.Read(ref ended) != 0) return;
                var store = previous.Breakpoints;
                previous.Breakpoints = null;
                previous.Dispose();
                session = new SoftDebuggerSession { Breakpoints = store };
                ConfigureSession(session);
                breakpoints.Bind(session, message => Protocol.SendEvent(message));
                dataBreakpoints.Invalidate(message => Protocol.SendEvent(message));
                dataBreakpoints = new GuestDataBreakpoints(session, Allocate);
                Interlocked.Exchange(ref pauseRequested, 0);
                Interlocked.Exchange(ref pauseStep, null);
                connection = new GuestConnection(endpoint!, Fail);
                session.Run(new SoftDebuggerStartInfo(connection), new DebuggerSessionOptions { EvaluationOptions = EvaluationOptions.DefaultOptions });
            }
        }
        catch (Exception error) { Fail(error); }
    }

    internal void Stop()
    {
        if (Interlocked.Exchange(ref stopping, 1) != 0) return;
        Interlocked.Exchange(ref attach, null)?.SetError(new ProtocolException("The local debug session was stopped."));
        lock (lifecycle)
        {
            owner?.Close();
        }
        Protocol.Stop();
    }

    /// <summary>Closes the guest transport and observes the connection callback before releasing the backend.</summary>
    public async ValueTask DisposeAsync()
    {
        if (Interlocked.Exchange(ref disposed, 1) != 0) return;
        Stop();
        Interlocked.Exchange(ref paused, null)?.Dispose();
        await opening;
        await reconnecting;
        if (owner is not null) await owner.DisposeAsync();
        if (connection is not null) await connection.DisposeAsync();
        session.Dispose();
    }
}
