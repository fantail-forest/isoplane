using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Protocol;
using Newtonsoft.Json;
using Newtonsoft.Json.Linq;

namespace Isoplane.Debugging;

/// <summary>Forwards native debugger messages while preserving the semantics of suspended managed frames.</summary>
public static class NativeDebugSession
{
    /// <summary>Relays one editor connection until either peer closes or cancellation stops both protocol readers.</summary>
    public static async Task RunAsync(Stream editorInput, Stream editorOutput, Stream debuggerInput, Stream debuggerOutput, CancellationToken cancellation)
    {
        using var input = new DebuggerInput(editorInput);
        var editor = new DebugProtocolClient(input, editorOutput, false, DebugProtocolOptions.AllowWildcardRegistrations);
        var backend = new DebugProtocolHost(debuggerInput, debuggerOutput, false, DebugProtocolOptions.AllowWildcardRegistrations);
        var state = new NativeSuspendedFrames();
        var gate = new object();
        var failure = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        editor.DispatcherError += (_, error) => failure.TrySetException(error.Exception);
        backend.DispatcherError += (_, error) => failure.TrySetException(error.Exception);
        editor.RegisterRequestType<Request, Arguments, Response>(request =>
        {
            try
            {
                var arguments = request.Arguments?.Data ?? new Dictionary<string, JToken>();
                if (request.Command == "cancel") throw new ProtocolException("Native debugger request cancellation is not supported by this session.");
                lock (gate) state.Validate(request.Command, arguments);
                var forwarded = new Request(request.Command);
                forwarded.Args.Data = arguments;
                backend.SendRequest(forwarded, (_, response) =>
                {
                    var body = response ?? new Response();
                    if (request.Command == "initialize") body.Data["supportsCancelRequest"] = false;
                    lock (gate) state.Inspect(request.Command, arguments, body.Data);
                    request.SetResponse(body);
                }, (_, error) => request.SetError(error));
            }
            catch (ProtocolException error) { request.SetError(error); }
        });
        backend.RegisterClientRequestType<ClientRequest, Arguments, Response>(request =>
        {
            var forwarded = new ClientRequest(request.Command);
            forwarded.Args.Data = request.Arguments?.Data ?? new Dictionary<string, JToken>();
            editor.SendClientRequest(forwarded, (_, response) => request.SetResponse(response ?? new Response()), (_, error) => request.SetError(error));
        });
        backend.RegisterEventType<Event>(message =>
        {
            if (message.Type is "continued" or "terminated") lock (gate) state.Clear();
            editor.SendEvent(message);
        });
        cancellation.ThrowIfCancellationRequested();
        editor.Run();
        backend.Run();
        using var stop = cancellation.Register(() => { editor.Stop(); backend.Stop(); });
        var editorEnded = Task.Run(editor.WaitForReader, CancellationToken.None);
        var backendEnded = Task.Run(backend.WaitForReader, CancellationToken.None);
        try { await await Task.WhenAny(editorEnded, backendEnded, failure.Task); }
        finally
        {
            editor.Stop(5000);
            backend.Stop(5000);
            await Task.WhenAll(editorEnded, backendEnded);
        }
    }

    private sealed class Arguments : DebugRequestArguments
    {
        [JsonExtensionData] public IDictionary<string, JToken> Data { get; set; } = new Dictionary<string, JToken>();
    }

    private sealed class Response : ResponseBody
    {
        [JsonExtensionData] public IDictionary<string, JToken> Data { get; set; } = new Dictionary<string, JToken>();
    }

    private sealed class Request : DebugRequestWithResponse<Arguments, Response>
    {
        public Request() : base("*") { }
        internal Request(string command) : base(command) { }
    }

    private sealed class ClientRequest : DebugClientRequestWithResponse<Arguments, Response>
    {
        public ClientRequest() : base("*") { }
        internal ClientRequest(string command) : base(command) { }
    }

    private sealed class Event : DebugEvent
    {
        public Event() : base("*") { }
        internal Event(string name) : base(name) { }
        [JsonExtensionData] public IDictionary<string, JToken> Data { get; set; } = new Dictionary<string, JToken>();
    }
}
