using System.Net;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Mono.Debugging.Client;
using Mono.Debugging.Soft;
using Newtonsoft.Json.Linq;
using Dap = Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;

namespace Isoplane.Debugging;

internal sealed partial class GuestDebugAdapter : DebugAdapterBase, IAsyncDisposable
{
    private SoftDebuggerSession session = new() { Breakpoints = new BreakpointStore() };
    private readonly TextWriter diagnostics;
    private readonly SourcePaths sources = new();
    private readonly GuestBreakpoints breakpoints;
    private GuestDataBreakpoints dataBreakpoints;
    private IPEndPoint? endpoint;
    private Task reconnecting = Task.CompletedTask;
    private readonly object lifecycle = new();
    private GuestConnection? connection;
    private GuestSession? owner;
    private string? package;
    private Task opening = Task.CompletedTask;
    private ConnectionRequest? attach;
    private readonly DebugLauncher? launcher;
    private readonly CancellationTokenSource launchCancellation = new();
    private Task launching = Task.CompletedTask;
    private Task disconnecting = Task.CompletedTask;
    private int detaching;
    private int detachmentPrepared;
    private bool launched;
    private bool supportsTerminal;
    private bool launchInTerminal;
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

    internal GuestDebugAdapter(Stream input, Stream output, TextWriter diagnostics, DebugLauncher? launcher = null)
    {
        this.diagnostics = diagnostics;
        this.launcher = launcher;
        InitializeProtocolClient(input, output);
        Protocol.RegisterRequestType<PrepareDetachRequest, PrepareDetachArguments>(HandlePrepareDetach);
        Protocol.RegisterRequestType<HotReloadRequest, HotReloadArguments, HotReloadResponse>(HandleHotReload);
        ConfigureSession(session);
        dataBreakpoints = new GuestDataBreakpoints(session, Allocate);
        breakpoints = new GuestBreakpoints(session, Allocate, message => Protocol.SendEvent(message), message => Protocol.SendEvent(message));
    }

    private void ConfigureSession(SoftDebuggerSession backend)
    {
        backend.IgnoreInternalUserBreak = args =>
        {
            if (Volatile.Read(ref stopping) != 0 || Volatile.Read(ref pauseRequested) != 0) return false;
            using var state = new PausedValues(args, Allocate, backend, sources);
            return dataBreakpoints.IgnoreInternalStop(state, value => Protocol.SendEvent(value));
        };
        backend.TargetHitBreakpoint += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Breakpoint);
        backend.TargetInterrupted += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Pause);
        backend.TargetStopped += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Step);
        backend.TargetExceptionThrown += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Exception);
        backend.TargetUnhandledException += (_, args) => Stopped(args, StoppedEvent.ReasonValue.Exception);
        backend.TargetReady += (_, _) => Interlocked.Exchange(ref attach, null)?.Complete();
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
        supportsTerminal = args.SupportsRunInTerminalRequest == true;
        request.SetResponse(new InitializeResponse
        {
            SupportsConfigurationDoneRequest = true, SupportTerminateDebuggee = true,
            SupportsConditionalBreakpoints = true, SupportsHitConditionalBreakpoints = true, SupportsLogPoints = true,
            SupportsFunctionBreakpoints = true, SupportsEvaluateForHovers = true, SupportsSetVariable = true,
            SupportsSetExpression = true, SupportsExceptionInfoRequest = true, SupportsDataBreakpoints = true,
            SupportsDataBreakpointBytes = false, SupportsGotoTargetsRequest = true, SupportsBreakpointLocationsRequest = true,
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
        try { sources.Configure(options.GetValueOrDefault("sourceFileMap")); }
        catch (ProtocolException error) { request.SetError(error); return; }
        owner = new GuestSession(SessionEnded, PackageChanged);
        attach = new ConnectionRequest(() => request.SetResponse(new AttachResponse()), request.SetError);
        opening = OpenAsync(new IPEndPoint(IPAddress.Loopback, (int)port.Value));
    }

    protected override void HandleLaunchRequestAsync(IRequestResponder<LaunchArguments> request)
    {
        if (!initialized)
        {
            request.SetError(new ProtocolException("Initialize the debug adapter before launching."));
            return;
        }
        if (owner is not null)
        {
            request.SetError(new ProtocolException("This adapter already owns a debug session."));
            return;
        }
        if (launcher is null)
        {
            request.SetError(new ProtocolException("Launch requires the installed Isoplane CLI debug adapter."));
            return;
        }
        if (request.Arguments.NoDebug == true)
        {
            request.SetError(new ProtocolException("Use isoplane dev to run without debugging."));
            return;
        }
        var properties = request.Arguments.ConfigurationProperties;
        string? project = properties.TryGetValue("project", out var projectValue) && projectValue.Type == JTokenType.String ? projectValue.Value<string>() : null;
        string? configuration = properties.TryGetValue("configuration", out var configurationValue)
            ? configurationValue.Type == JTokenType.String ? configurationValue.Value<string>() : null : "Debug";
        bool? watch = properties.TryGetValue("watch", out var watchValue)
            ? watchValue.Type == JTokenType.Boolean ? watchValue.Value<bool>() : null : true;
        long? port = properties.TryGetValue("port", out var portValue) && portValue.Type == JTokenType.Integer ? portValue.Value<long>() : null;
        if (string.IsNullOrWhiteSpace(project) || !Path.IsPathFullyQualified(project) || string.IsNullOrWhiteSpace(configuration) || watch is null
            || properties.ContainsKey("port") && (port is null or < 0 or > 65535))
        {
            request.SetError(new ProtocolException("Launch requires an absolute project directory, a build configuration, an optional HTTP port from 0 to 65535, and a boolean watch option."));
            return;
        }
        string? console = properties.TryGetValue("console", out var consoleValue)
            ? consoleValue.Type == JTokenType.String ? consoleValue.Value<string>() : null
            : supportsTerminal ? "integratedTerminal" : "internalConsole";
        if (console is not ("integratedTerminal" or "internalConsole") || console == "integratedTerminal" && !supportsTerminal)
        {
            request.SetError(new ProtocolException("Launch console must be internalConsole, or integratedTerminal when the editor supports runInTerminal."));
            return;
        }
        try { sources.Configure(properties.GetValueOrDefault("sourceFileMap")); }
        catch (ProtocolException error) { request.SetError(error); return; }
        launchInTerminal = console == "integratedTerminal";
        owner = new GuestSession(SessionEnded, PackageChanged);
        attach = new ConnectionRequest(() => request.SetResponse(new LaunchResponse()), request.SetError);
        launched = true;
        launching = LaunchAsync(new DebugLaunchOptions(project, configuration, port is { } number ? (ushort)number : null, watch.Value));
    }

    private async Task LaunchAsync(DebugLaunchOptions options)
    {
        try
        {
            await launcher!.RunAsync(options,
                address => opening = OpenAsync(address, completeOnOpen: true),
                text => Protocol.SendEvent(new OutputEvent { Category = OutputEvent.CategoryValue.Console, Output = text + Environment.NewLine }),
                launchInTerminal ? RunInTerminalAsync : null, launchCancellation.Token);
            SessionEnded();
        }
        catch (OperationCanceledException) when (launchCancellation.IsCancellationRequested) { return; }
        catch (Exception error) { Fail(error); }
    }

    private async Task RunInTerminalAsync(DebugTerminal terminal, CancellationToken cancellation)
    {
        var completed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        using var cancelled = cancellation.Register(() => completed.TrySetCanceled(cancellation));
        Protocol.SendClientRequest(new RunInTerminalRequest(terminal.Directory, terminal.Arguments.ToList())
        {
            Kind = RunInTerminalArguments.KindValue.Integrated,
            Title = "Isoplane"
        }, (_, _) => completed.TrySetResult(), (_, error) => completed.TrySetException(error));
        await completed.Task;
    }

    private async Task OpenAsync(IPEndPoint address, bool completeOnOpen = false)
    {
        try
        {
            var target = await owner!.ConnectAsync(address);
            var application = ReadSymbols(target.Package);
            await OpenHotReloadAsync(target.Package);
            lock (lifecycle)
            {
                if (Volatile.Read(ref stopping) != 0) return;
                endpoint = target.Guests;
                package = target.Package;
                if (application is not null) breakpoints.Rebuilt(application);
                connection = new GuestConnection(target.Guests, Fail);
                Protocol.SendEvent(new InitializedEvent());
                if (completeOnOpen) Interlocked.Exchange(ref attach, null)?.Complete();
            }
        }
        catch (Exception error) { Fail(error); }
    }

    /// <summary>The session rebuilt its package after a source edit; the next guest runs the code these symbols describe.</summary>
    private void PackageChanged()
    {
        if (package is not { } rebuilt || ReadSymbols(rebuilt) is not { } application) return;
        lock (lifecycle)
        {
            if (Volatile.Read(ref stopping) == 0) breakpoints.Rebuilt(application);
        }
    }

    /// <summary>Reads the application's symbols, or returns null and keeps the previous ones when the package cannot be read.</summary>
    private ApplicationSymbols? ReadSymbols(string path)
    {
        try { return ApplicationSymbols.Read(path); }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidDataException)
        {
            // Breakpoints then show what the last readable build, or a running guest, says about them.
            diagnostics.WriteLine($"isoplane: could not read application symbols from {path}: {error.Message}");
            return null;
        }
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

    protected override void HandleSetBreakpointsRequestAsync(IRequestResponder<SetBreakpointsArguments, SetBreakpointsResponse> request)
    {
        lock (lifecycle)
        {
            if (Volatile.Read(ref applyingHotReload) != 0) pendingSourceBreakpoints.Enqueue(request);
            else SetSourceBreakpoints(request);
        }
    }

    private void SetSourceBreakpoints(IRequestResponder<SetBreakpointsArguments, SetBreakpointsResponse> request)
    {
        try { request.SetResponse(breakpoints.SetSource(request.Arguments)); }
        catch (Exception error) { request.SetError(new ProtocolException(error.Message, error)); }
    }
    protected override BreakpointLocationsResponse HandleBreakpointLocationsRequest(BreakpointLocationsArguments arguments)
    {
        RequireNoHotReload();
        lock (lifecycle) return breakpoints.Locations(arguments);
    }

    protected override SetFunctionBreakpointsResponse HandleSetFunctionBreakpointsRequest(SetFunctionBreakpointsArguments arguments)
    {
        RequireNoHotReload();
        lock (lifecycle) return breakpoints.SetFunctions(arguments);
    }
    protected override SetExceptionBreakpointsResponse HandleSetExceptionBreakpointsRequest(SetExceptionBreakpointsArguments arguments)
    {
        RequireNoHotReload();
        lock (lifecycle) return breakpoints.SetExceptions(arguments);
    }

    protected override DataBreakpointInfoResponse HandleDataBreakpointInfoRequest(DataBreakpointInfoArguments arguments) => dataBreakpoints.Describe(Pause, arguments);
    protected override SetDataBreakpointsResponse HandleSetDataBreakpointsRequest(SetDataBreakpointsArguments arguments)
    {
        RequireNoHotReload();
        return dataBreakpoints.Set(arguments);
    }

    protected override ThreadsResponse HandleThreadsRequest(ThreadsArguments arguments) => new()
    {
        Threads = session.GetProcesses().SelectMany(process => process.GetThreads())
            .Select(thread => new Dap.Thread { Id = checked((int)thread.Id), Name = thread.Name }).ToList()
    };

    private PausedValues Pause
    {
        get
        {
            RequireNoHotReload();
            return Volatile.Read(ref paused) ?? throw new ProtocolException("The guest is not paused.");
        }
    }
    private int Allocate() => Interlocked.Increment(ref nextHandle);
    protected override StackTraceResponse HandleStackTraceRequest(StackTraceArguments arguments) => Pause.StackTrace(arguments);
    protected override ScopesResponse HandleScopesRequest(ScopesArguments arguments) => Pause.Scopes(arguments.FrameId);
    protected override VariablesResponse HandleVariablesRequest(VariablesArguments arguments) => Pause.Variables(arguments.VariablesReference);

    protected override EvaluateResponse HandleEvaluateRequest(EvaluateArguments arguments) => Pause.Evaluate(arguments);
    protected override SetVariableResponse HandleSetVariableRequest(SetVariableArguments arguments) => Pause.SetVariable(arguments);
    protected override SetExpressionResponse HandleSetExpressionRequest(SetExpressionArguments arguments) => Pause.SetExpression(arguments);
    protected override ExceptionInfoResponse HandleExceptionInfoRequest(ExceptionInfoArguments arguments) => Pause.ExceptionInfo(arguments.ThreadId);

    protected override GotoTargetsResponse HandleGotoTargetsRequest(GotoTargetsArguments arguments) => Pause.GotoTargets(arguments);

    protected override GotoResponse HandleGotoRequest(GotoArguments arguments)
    {
        Stopped(Pause.Goto(arguments), StoppedEvent.ReasonValue.Goto);
        return new GotoResponse();
    }

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
        RequireNoHotReload();
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
        lock (lifecycle)
        {
            if (Volatile.Read(ref applyingHotReload) != 0)
                pendingHotReloadStops.Enqueue((args, reason));
            else PresentStop(args, reason);
        }
    }

    private void PresentStop(TargetEventArgs args, StoppedEvent.ReasonValue reason)
    {
        if (Volatile.Read(ref stopping) != 0) return;
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
        var state = new PausedValues(args, Allocate, session, sources);
        if (!breakpoints.ShouldStop(state.IsUnhandled))
        {
            state.Dispose();
            session.Continue();
            return;
        }
        if (state.IsUnhandled) reason = StoppedEvent.ReasonValue.Exception;
        var hits = dataBreakpoints.TakeHits(state, value => Protocol.SendEvent(value));
        if (hits.IsDataStop && reason != StoppedEvent.ReasonValue.Pause)
        {
            if (hits.BreakpointIds.Length == 0)
            {
                state.Dispose();
                session.Continue();
                return;
            }
            reason = StoppedEvent.ReasonValue.DataBreakpoint;
        }
        Interlocked.Exchange(ref paused, state)?.Dispose();
        Protocol.SendEvent(new StoppedEvent { Reason = reason, ThreadId = state.ThreadId, AllThreadsStopped = true, HitBreakpointIds = hits.BreakpointIds.ToList(), Description = hits.Description });
    }

    private void HandlePrepareDetach(IRequestResponder<PrepareDetachArguments> request)
    {
        RequireNoHotReload();
        if (owner is null || launched && !launchInTerminal || Interlocked.Exchange(ref detaching, 1) != 0)
        {
            request.SetError(new ProtocolException("Select an attached or terminal-launched session that is not already detaching."));
            return;
        }
        disconnecting = PrepareDetachAsync(request);
    }

    private async Task PrepareDetachAsync(IRequestResponder<PrepareDetachArguments> request)
    {
        try
        {
            await owner!.PrepareDetachAsync();
            Volatile.Write(ref detachmentPrepared, 1);
            request.SetResponse(new PrepareDetachResponse());
        }
        catch (Exception error)
        {
            Interlocked.Exchange(ref detaching, 0);
            if (Volatile.Read(ref stopping) != 0)
                diagnostics.WriteLine($"isoplane debugger: detach preparation ended during shutdown: {error.Message}");
            else
            {
                request.SetError(new ProtocolException($"Could not prepare debugger detachment: {error.Message}", error));
                lock (lifecycle)
                    if (session.HasExited && connection is not null) reconnecting = ReconnectAsync(session, connection);
            }
        }
    }

    protected override void HandleDisconnectRequestAsync(IRequestResponder<DisconnectArguments> request)
    {
        if (request.Arguments.TerminateDebuggee == false && Volatile.Read(ref ended) == 0)
        {
            RequireNoHotReload();
            if (launched && !launchInTerminal)
            {
                request.SetError(new ProtocolException("Detach requires a terminal-owned application. Launch with console: integratedTerminal or attach to isoplane dev --debug."));
                return;
            }
            bool prepared = Interlocked.Exchange(ref detachmentPrepared, 0) != 0;
            if (owner is null || !prepared && Interlocked.Exchange(ref detaching, 1) != 0)
            {
                request.SetError(new ProtocolException("No attached session is available to detach."));
                return;
            }
            disconnecting = DetachAsync(request, prepared);
            return;
        }
        request.SetResponse(new DisconnectResponse());
        Stop();
    }

    private async Task DetachAsync(IRequestResponder<DisconnectArguments> request, bool prepared)
    {
        try
        {
            if (!prepared) await owner!.PrepareDetachAsync();
            prepared = true;
            if (session.IsConnected && !session.HasExited)
            {
                try { await Task.Run(() => session.VirtualMachine.Detach()); }
                catch (Mono.Debugger.Soft.VMDisconnectedException)
                {
                    // Request completion can close the guest before its exit event reaches the adapter.
                }
            }
            else
                connection?.Close();
            await owner!.DetachAsync();
            request.SetResponse(new DisconnectResponse());
            Stop();
        }
        catch (Exception error)
        {
            Interlocked.Exchange(ref detaching, 0);
            if (Volatile.Read(ref stopping) != 0)
            {
                diagnostics.WriteLine($"isoplane debugger: detach ended during shutdown: {error.Message}");
                return;
            }
            request.SetError(new ProtocolException($"Could not detach the debugger: {error.Message}", error));
            if (prepared) Fail(error);
            else
            {
                lock (lifecycle)
                    if (session.HasExited && connection is not null) reconnecting = ReconnectAsync(session, connection);
            }
        }
    }

    protected override void HandleProtocolError(Exception error) => Fail(error);

    private void Fail(Exception error)
    {
        if (Volatile.Read(ref stopping) != 0 || Volatile.Read(ref ended) != 0) return;
        Interlocked.Exchange(ref failed, 1);
        diagnostics.WriteLine($"isoplane debugger: {error}");
        Protocol.SendEvent(new OutputEvent { Category = OutputEvent.CategoryValue.Stderr, Output = $"Isoplane debugger stopped: {error.Message}{Environment.NewLine}" });
        Interlocked.Exchange(ref attach, null)?.Reject(new ProtocolException(error.Message));
        Stop();
    }

    private void SessionEnded()
    {
        lock (lifecycle)
        {
            if (Interlocked.Exchange(ref ended, 1) != 0 || Volatile.Read(ref stopping) != 0) return;
            Interlocked.Exchange(ref paused, null)?.Dispose();
            Interlocked.Exchange(ref attach, null)?.Reject(new ProtocolException("The local development session ended."));
            connection?.Close();
            Protocol.SendEvent(new TerminatedEvent());
            launchCancellation.Cancel();
        }
    }

    private void Ended()
    {
        lock (lifecycle)
        {
            Interlocked.Exchange(ref paused, null)?.Dispose();
            if (Volatile.Read(ref stopping) != 0 || Volatile.Read(ref ended) != 0 || Volatile.Read(ref detaching) != 0) return;
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
            await applyingChanges;
            await previousConnection.DisposeAsync();
            if (package is { } currentPackage) await OpenHotReloadAsync(currentPackage);
            lock (lifecycle)
            {
                if (Volatile.Read(ref stopping) != 0 || Volatile.Read(ref ended) != 0 || Volatile.Read(ref detaching) != 0) return;
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
        Interlocked.Exchange(ref attach, null)?.Reject(new ProtocolException("The local debug session was stopped."));
        launchCancellation.Cancel();
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
        await disconnecting;
        await launching;
        await opening;
        await reconnecting;
        await applyingChanges;
        if (hotReload is not null) await hotReload.DisposeAsync();
        if (owner is not null) await owner.DisposeAsync();
        if (connection is not null) await connection.DisposeAsync();
        session.Dispose();
        launchCancellation.Dispose();
    }

    private sealed class PrepareDetachResponse : ResponseBody;

    private sealed class PrepareDetachArguments : DebugRequestArguments;

    private sealed class PrepareDetachRequest() : DebugRequest<PrepareDetachArguments>("isoplane/prepareDetach");

    private sealed record ConnectionRequest(Action Complete, Action<ProtocolException> Reject);
}
