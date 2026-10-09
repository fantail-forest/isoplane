using Isoplane.Build;
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
        if (hotReload is null)
        {
            request.SetError(new ProtocolException(hotReloadError ?? "This session has no Hot Reload project. Launch or attach to a source project's local debug session."));
            return;
        }
        var state = Volatile.Read(ref paused);
        if (state is null)
        {
            request.SetError(new ProtocolException("Pause the guest before applying source edits."));
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
        applyingChanges = ApplyHotReloadAsync(request, state, edits);
    }

    private async Task ApplyHotReloadAsync(IRequestResponder<HotReloadArguments, HotReloadResponse> request, PausedValues original, SourceUpdate[] edits)
    {
        bool ownsOperation = true;
        try
        {
            var frames = session.VirtualMachine.GetThreads().Single(thread => thread.Id == original.ThreadId).GetFrames();
            var remapping = ActiveStatementRemapping.Capture(frames[0]);
            var callers = frames.Skip(1).Select(CallerRemapping.Capture).OfType<CallerRemapping>().ToArray();
            var reply = await hotReload!.ApplyAsync(session.VirtualMachine, edits, sources, launchCancellation.Token);
            bool applied = reply.Pending && !reply.RestartRequired && reply.Diagnostics.Length == 0;
            if (applied)
            {
                if (!ReferenceEquals(original, Volatile.Read(ref paused)))
                    throw new ProtocolException("The guest ended its pause while Hot Reload was being applied.");
                breakpoints.RefreshAfterHotReload(edits.Select(edit => edit.Path), sources);
                foreach (var caller in callers) caller.Apply(reply.Updates);
                var refreshed = session.RefreshAfterHotReload(remapping?.Resolve(reply.Updates));
                var state = new PausedValues(refreshed, Allocate, session, sources);
                Interlocked.Exchange(ref paused, state)?.Dispose();
                ownsOperation = false;
                CompleteHotReload();
                Protocol.SendEvent(new StoppedEvent { Reason = StoppedEvent.ReasonValue.Pause, ThreadId = state.ThreadId, AllThreadsStopped = true });
            }
            if (ownsOperation)
            {
                ownsOperation = false;
                CompleteHotReload();
            }
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
