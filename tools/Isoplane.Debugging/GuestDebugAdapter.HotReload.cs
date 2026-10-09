using Isoplane.Build;
using Mono.Debugging.Client;
using Newtonsoft.Json;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;

namespace Isoplane.Debugging;

internal sealed partial class GuestDebugAdapter
{
    private HotReloadSession? hotReload;
    private string? hotReloadError;
    private Task applyingChanges = Task.CompletedTask;
    private int applyingHotReload;
    private readonly Queue<(TargetEventArgs Arguments, StoppedEvent.ReasonValue Reason)> pendingHotReloadStops = new();
    private readonly Queue<IRequestResponder<SetBreakpointsArguments, SetBreakpointsResponse>> pendingSourceBreakpoints = new();

    private async Task OpenHotReloadAsync(string applicationPackage)
    {
        if (hotReload is not null)
        {
            await hotReload.DisposeAsync();
            hotReload = null;
        }
        hotReloadError = null;
        if (owner?.Project is not { } project) return;
        try
        {
            hotReload = await HotReloadSession.StartAsync(project, applicationPackage, launchCancellation.Token);
            Protocol.SendEvent(new HotReloadReadyEvent { Documents = hotReload.Documents });
        }
        catch (Exception error) when (error is IOException or InvalidOperationException or ArgumentException)
        {
            hotReloadError = "Could not prepare Hot Reload: " + error.Message;
            Protocol.SendEvent(new OutputEvent { Category = OutputEvent.CategoryValue.Stderr, Output = hotReloadError + Environment.NewLine });
        }
    }

    private void RequireNoHotReload()
    {
        if (Volatile.Read(ref applyingHotReload) != 0)
            throw new ProtocolException("Wait for Hot Reload to finish before inspecting or changing execution.");
    }

    private void HandleHotReload(IRequestResponder<HotReloadArguments, HotReloadResponse> request)
    {
        lock (lifecycle)
        {
            if (hotReload is null)
            {
                request.SetError(new ProtocolException(hotReloadError ?? "This session has no Hot Reload project. Launch or attach to a source project's local debug session."));
                return;
            }
            if (!session.IsConnected || session.HasExited)
            {
                request.SetError(new ProtocolException("Hot Reload requires an active guest invocation."));
                return;
            }
            if (request.Arguments.Sources is not { Length: > 0 } edits
                || edits.Any(edit => edit is null || !Path.IsPathFullyQualified(edit.Path ?? "") || edit.Text is null))
            {
                request.SetError(new ProtocolException("Hot Reload requires source text and an absolute path for each edited document."));
                return;
            }
            edits = edits.Select(edit => edit with { Path = Path.GetFullPath(edit.Path) }).ToArray();
            if (Interlocked.CompareExchange(ref applyingHotReload, 1, 0) != 0)
            {
                request.SetError(new ProtocolException("A Hot Reload operation is already in progress."));
                return;
            }
            applyingChanges = ApplyHotReloadAsync(request, edits);
        }
    }

    private async Task ApplyHotReloadAsync(IRequestResponder<HotReloadArguments, HotReloadResponse> request, SourceUpdate[] edits)
    {
        bool ownsOperation = true;
        try
        {
            CompilerReply reply;
            bool applied;
            PausedValues? refreshedState = null;
            var machine = session.VirtualMachine;
            machine.Suspend();
            bool safeToResume = true;
            try
            {
                var original = Volatile.Read(ref paused);
                var frames = session.VirtualMachine.GetThreads().Single().GetFrames();
                var remapping = original is not null ? ActiveStatementRemapping.Capture(frames[0]) : null;
                var callers = frames.Skip(1).Select(CallerRemapping.Capture).OfType<CallerRemapping>().ToArray();
                reply = await hotReload!.ApplyAsync(session.VirtualMachine, edits, sources, launchCancellation.Token);
                applied = reply.Pending && !reply.RestartRequired && reply.Diagnostics.Length == 0;
                if (applied)
                {
                    if (!ReferenceEquals(original, Volatile.Read(ref paused)))
                        throw new ProtocolException("The guest changed execution state while Hot Reload was being applied.");
                    breakpoints.RefreshAfterHotReload(edits.Select(edit => edit.Path), sources);
                    foreach (var caller in callers) caller.Apply(reply.Updates);
                    if (original is null)
                        session.RefreshSourceBreakpointsAfterHotReload();
                    else
                    {
                        var refreshed = session.RefreshAfterHotReload(remapping?.Resolve(reply.Updates));
                        refreshedState = new PausedValues(refreshed, Allocate, session, sources);
                        Interlocked.Exchange(ref paused, refreshedState)?.Dispose();
                    }
                }
            }
            catch (HotReloadApplicationException)
            {
                safeToResume = false;
                throw;
            }
            finally
            {
                if (safeToResume && Volatile.Read(ref stopping) == 0 && !session.HasExited) machine.Resume();
            }
            ownsOperation = false;
            CompleteHotReload();
            if (refreshedState is { } state)
                Protocol.SendEvent(new StoppedEvent { Reason = StoppedEvent.ReasonValue.Pause, ThreadId = state.ThreadId, AllThreadsStopped = true });
            request.SetResponse(new HotReloadResponse
            {
                AppliedModules = applied ? reply.Updates.Length : 0,
                Diagnostics = reply.Diagnostics,
                RestartRequired = reply.RestartRequired
            });
        }
        catch (HotReloadApplicationException error)
        {
            request.SetError(new ProtocolException(error.Message, error));
            Fail(error);
        }
        catch (Exception error)
        {
            diagnostics.WriteLine($"isoplane: applying source edits: {error}");
            request.SetError(new ProtocolException(error.Message, error));
        }
        finally
        {
            if (ownsOperation) CompleteHotReload();
        }
    }

    private void CompleteHotReload()
    {
        lock (lifecycle)
        {
            while (pendingSourceBreakpoints.TryDequeue(out var request))
            {
                if (Volatile.Read(ref stopping) != 0 || Volatile.Read(ref failed) != 0)
                    request.SetError(new ProtocolException("The debug session ended before the source breakpoints could be applied."));
                else SetSourceBreakpoints(request);
            }
            Interlocked.Exchange(ref applyingHotReload, 0);
            while (pendingHotReloadStops.TryDequeue(out var stopped))
            {
                if (Volatile.Read(ref stopping) != 0 || session.HasExited) continue;
                stopped.Arguments.Backtrace = ExecutionPosition.Refresh(session).Backtrace;
                PresentStop(stopped.Arguments, stopped.Reason);
            }
        }
    }

    private sealed class HotReloadReadyEvent() : DebugEvent("isoplane/hotReloadReady")
    {
        [JsonProperty("documents")]
        public string[] Documents { get; set; } = [];
    }

    private sealed class HotReloadArguments : DebugRequestArguments
    {
        [JsonProperty("sources")]
        public SourceUpdate[] Sources { get; set; } = [];
    }

    private sealed class HotReloadResponse : ResponseBody
    {
        [JsonProperty("appliedModules")]
        public int AppliedModules { get; set; }
        [JsonProperty("diagnostics")]
        public string[] Diagnostics { get; set; } = [];
        [JsonProperty("restartRequired")]
        public bool RestartRequired { get; set; }
    }

    private sealed class HotReloadRequest() : DebugRequestWithResponse<HotReloadArguments, HotReloadResponse>("isoplane/hotReload");
}
