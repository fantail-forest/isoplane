using System.Net;
using System.Net.Sockets;
using Isoplane.Debugging;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol;
using Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages;
using Newtonsoft.Json.Linq;
using Xunit;

namespace Isoplane.Debugging.Tests;

public sealed class DebugAdapterTests
{
    [Fact]
    public async Task initialization_offers_managed_watchpoints_without_raw_memory_controls()
    {
        await using var client = await Client.StartAsync();
        var capabilities = await client.Send(new InitializeRequest());
        Assert.True(capabilities.SupportsDataBreakpoints == true, "The editor must offer managed watchpoints.");
        Assert.True(capabilities.SupportsDataBreakpointBytes == false, "The editor must hide unsupported address and byte-count watchpoint controls.");
    }

    [Theory]
    [InlineData("192.168.0.1", 1234)]
    [InlineData("::1", 1234)]
    [InlineData("127.0.0.1", 0)]
    [InlineData("127.0.0.1", 65536)]
    public async Task attach_rejects_endpoints_outside_the_owned_loopback_shape(string address, int port)
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(Attach(address, port)));
        Assert.True(error.Message.Contains("session-owned address", StringComparison.Ordinal), "Unsafe endpoints must identify the session-owned loopback requirement.");
    }

    [Theory]
    [InlineData(false, true)]
    [InlineData(true, false)]
    public async Task unsupported_source_coordinates_fail_before_attachment(bool lines, bool columns)
    {
        await using var client = await Client.StartAsync();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new InitializeRequest
        {
            Args = { LinesStartAt1 = lines, ColumnsStartAt1 = columns }
        }));
        Assert.True(error.Message.Contains("one-based", StringComparison.Ordinal), "Unsupported editor coordinates must receive an actionable protocol error.");
    }

    [Fact]
    public async Task a_second_initialization_is_rejected_without_losing_the_connection()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Initialize());
        Assert.True(error.Message.Contains("already initialized", StringComparison.Ordinal), "Initialization must describe the duplicate request.");
        Assert.False(client.Adapter.Failed, "An invalid editor request must not become a fatal adapter error.");
    }

    [Fact]
    public async Task attachment_requires_initialization()
    {
        await using var client = await Client.StartAsync();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(Attach("127.0.0.1", 1234)));
        Assert.True(error.Message.Contains("Initialize", StringComparison.Ordinal), "Attachment must require the editor handshake first.");
    }

    [Fact]
    public async Task detachment_requires_an_attached_session()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new DisconnectRequest { Args = { TerminateDebuggee = false } }));
        Assert.True(error.Message.Contains("No attached session", StringComparison.Ordinal), "Detachment requires a session whose ownership can be released.");
    }

    [Fact]
    public async Task configuration_requires_an_attached_session()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new ConfigurationDoneRequest()));
        Assert.True(error.Message.Contains("attached session", StringComparison.Ordinal), "Configuration cannot start a guest before an endpoint is supplied.");
    }

    [Theory]
    [InlineData("relative.cs", 1)]
    [InlineData("/application.cs", 0)]
    public async Task invalid_breakpoint_locations_are_rejected(string source, int line)
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new SetBreakpointsRequest
        {
            Args = { Source = new Source { Path = source }, Breakpoints = [new SourceBreakpoint { Line = line }] }
        }));
        Assert.True(error.Message.Contains("absolute", StringComparison.Ordinal) || error.Message.Contains("positive line", StringComparison.Ordinal),
            "Invalid breakpoints must be diagnosed before changing the guest.");
    }

    [Theory]
    [InlineData("0")]
    [InlineData("-1")]
    [InlineData("a + 1")]
    [InlineData("999999999999999999999999")]
    public async Task malformed_hit_conditions_receive_an_editor_error(string condition)
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new SetBreakpointsRequest
        {
            Args = { Source = new Source { Path = Path.GetFullPath("Application.cs") }, Breakpoints = [new SourceBreakpoint { Line = 1, HitCondition = condition }] }
        }));
        Assert.True(error.Message.Contains("positive count", StringComparison.Ordinal), "Invalid hit conditions must explain the supported syntax.");
    }

    [Fact]
    public async Task function_breakpoints_require_a_qualified_method()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new SetFunctionBreakpointsRequest
        {
            Args = { Breakpoints = [new Microsoft.VisualStudio.Shared.VSCodeDebugProtocol.Messages.FunctionBreakpoint { Name = "Method" }] }
        }));
        Assert.True(error.Message.Contains("Type.Method", StringComparison.Ordinal), "A function breakpoint must identify its declaring type.");
    }

    [Fact]
    public async Task unknown_exception_filters_are_not_silently_ignored()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new SetExceptionBreakpointsRequest { Args = { Filters = ["unknown"] } }));
        Assert.True(error.Message.Contains("all or uncaught", StringComparison.Ordinal), "The debugger must name its supported exception filters.");
    }

    [Fact]
    public async Task variables_cannot_be_read_while_the_guest_is_not_paused()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new VariablesRequest { Args = { VariablesReference = 1 } }));
        Assert.True(error.Message.Contains("not paused", StringComparison.Ordinal), "Variable access must require a current pause.");
    }

    [Fact]
    public async Task data_breakpoint_inspection_requires_a_current_pause()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new DataBreakpointInfoRequest { Args = { Name = "sample.Value" } }));
        Assert.True(error.Message.Contains("not paused", StringComparison.Ordinal), "A storage location cannot be resolved without a guest frame.");
    }

    [Fact]
    public async Task empty_data_breakpoints_can_be_configured_before_the_guest_starts()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var result = await client.Send(new SetDataBreakpointsRequest { Args = { Breakpoints = [] } });
        Assert.True(result.Breakpoints.Count == 0, "Initial editor configuration must accept an empty data breakpoint set.");
    }

    [Fact]
    public async Task data_breakpoints_reject_locations_from_another_session()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new SetDataBreakpointsRequest
        {
            Args = { Breakpoints = [new DataBreakpoint { DataId = "stale" }] }
        }));
        Assert.True(error.Message.Contains("no longer available", StringComparison.Ordinal), "A stale data identifier must request a fresh selection.");
    }

    [Theory]
    [InlineData(DataBreakpointAccessType.Read)]
    [InlineData(DataBreakpointAccessType.ReadWrite)]
    public async Task data_breakpoints_do_not_claim_read_watchpoint_support(DataBreakpointAccessType access)
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new SetDataBreakpointsRequest
        {
            Args = { Breakpoints = [new DataBreakpoint { DataId = "stale", AccessType = access }] }
        }));
        Assert.True(error.Message.Contains("read watchpoints", StringComparison.Ordinal), "Read access must receive an explicit unsupported response.");
    }

    [Fact]
    public async Task pause_requires_a_running_guest()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        var error = await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new PauseRequest { Args = { ThreadId = 1 } }));
        Assert.True(error.Message.Contains("running guest", StringComparison.Ordinal), "Pause must explain that no guest is running.");
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task an_owned_session_cannot_be_attached_or_started_twice(bool configure)
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        int port = ((IPEndPoint)listener.LocalEndpoint).Port;
        var attached = client.Send(Attach("127.0.0.1", port));
        using var control = await AcceptControlAsync(listener, client.Cancellation);
        await client.Initialized.Task.WaitAsync(client.Cancellation);
        if (configure) await client.Send(new ConfigurationDoneRequest());
        var error = await Assert.ThrowsAsync<ProtocolException>(() => configure
            ? client.Send(new ConfigurationDoneRequest()) : client.Send(Attach("127.0.0.1", port)));
        Assert.True(error.Message.Contains(configure ? "one attached session" : "already owns", StringComparison.Ordinal),
            "The adapter must reject an attempt to replace or restart its owned session.");
        client.Adapter.Stop();
        await Assert.ThrowsAsync<ProtocolException>(() => attached);
    }

    [Theory]
    [InlineData("adapter")]
    [InlineData("editor-default")]
    [InlineData("editor-terminate")]
    public async Task stopping_during_the_guest_handshake_closes_the_owned_connection(string stop)
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var endpoint = (IPEndPoint)listener.LocalEndpoint;
        var attached = client.Send(Attach("127.0.0.1", endpoint.Port));
        using var control = await AcceptControlAsync(listener, client.Cancellation);
        await client.Initialized.Task.WaitAsync(client.Cancellation);
        await client.Send(new ConfigurationDoneRequest());
        using var peer = await listener.AcceptTcpClientAsync(client.Cancellation);
        if (stop == "adapter") client.Adapter.Stop();
        else await client.Send(new DisconnectRequest { Args = { TerminateDebuggee = stop == "editor-terminate" ? true : null } });
        await client.Adapter.DisposeAsync().AsTask().WaitAsync(client.Cancellation);
        try { await attached; }
        catch (ProtocolException error) { client.Diagnostics.WriteLine("Expected cancelled attachment: " + error.Message); }
        var buffer = new byte[64];
        try
        {
            int received;
            do { received = await peer.GetStream().ReadAsync(buffer, client.Cancellation); } while (received != 0);
        }
        catch (IOException error) { client.Diagnostics.WriteLine("Expected closed debugger transport: " + error.Message); }
        Assert.False(client.Adapter.Failed, "An explicitly stopped handshake must release ownership without reporting an unexpected adapter failure.");
    }

    [Theory]
    [InlineData("{\"version\":3,\"port\":1234,\"package\":\"/application.tar\"}")]
    [InlineData("{\"version\":2,\"port\":0,\"package\":\"/application.tar\"}")]
    [InlineData("{\"version\":2,\"port\":65536,\"package\":\"/application.tar\"}")]
    [InlineData("{\"version\":2,\"port\":1234}")]
    [InlineData("{\"version\":2,\"port\":1234,\"package\":\"application.tar\"}")]
    [InlineData("not-json")]
    [InlineData("null")]
    public async Task invalid_session_control_endpoints_fail_attachment(string message)
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var attached = client.Send(Attach("127.0.0.1", ((IPEndPoint)listener.LocalEndpoint).Port));
        using var control = await listener.AcceptTcpClientAsync(client.Cancellation);
        using var writer = new StreamWriter(control.GetStream(), leaveOpen: true) { AutoFlush = true };
        await writer.WriteLineAsync(message);
        await Assert.ThrowsAsync<ProtocolException>(() => attached);
        Assert.True(client.Adapter.Failed, "an invalid local-session handshake must not start a guest connection");
    }

    [Fact]
    public async Task a_breakpoint_is_verified_from_the_built_application_before_any_guest_runs()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-adapter-symbols-");
        try
        {
            string source = Path.Combine(directory.FullName, "Example.cs"), package = Path.Combine(directory.FullName, "application.tar");
            BuiltApplication.Write(package, source, "public static class Example\n{\n    public static int Run()\n    {\n\n        return 42;\n    }\n}\n");
            await using var client = await Client.StartAsync();
            await client.Initialize();
            using var listener = new TcpListener(IPAddress.Loopback, 0);
            listener.Start();
            var attached = client.Send(Attach("127.0.0.1", ((IPEndPoint)listener.LocalEndpoint).Port));
            using var control = await AcceptControlAsync(listener, client.Cancellation, package);
            await client.Initialized.Task.WaitAsync(client.Cancellation);

            var set = await client.Send(new SetBreakpointsRequest
            {
                Args = { Source = new Source { Path = source }, Breakpoints = [new SourceBreakpoint { Line = 6 }, new SourceBreakpoint { Line = 5 }] }
            });
            Assert.True(set.Breakpoints[0].Verified, "a breakpoint on a statement must be verified while no guest is running");
            Assert.False(set.Breakpoints[1].Verified, "a breakpoint on a line without a statement must stay unverified");
            Assert.Contains("No statement", set.Breakpoints[1].Message, StringComparison.Ordinal);
            var elsewhere = await client.Send(new SetBreakpointsRequest
            {
                Args = { Source = new Source { Path = Path.Combine(directory.FullName, "Other.cs") }, Breakpoints = [new SourceBreakpoint { Line = 6 }] }
            });
            Assert.False(elsewhere.Breakpoints[0].Verified, "a file the application was not built from is left to the guest to bind");

            // A source edit: the session rebuilds its package in place and says so.
            BuiltApplication.Write(package, source, "public static class Example\n{\n    public static int Run()\n    {\n        int answer = 42;\n        return answer;\n    }\n}\n");
            using var writer = new StreamWriter(control.GetStream(), leaveOpen: true) { AutoFlush = true };
            await writer.WriteLineAsync("{\"version\":2,\"action\":\"packageChanged\"}");
            Breakpoint changed;
            do { changed = await client.Breakpoints.Reader.ReadAsync(client.Cancellation); } while (changed.Id != set.Breakpoints[1].Id);
            Assert.True(changed.Verified, "a rebuild that gives the line a statement must verify its breakpoint");

            client.Adapter.Stop();
            await Assert.ThrowsAsync<ProtocolException>(() => attached);
        }
        finally { directory.Delete(recursive: true); }
    }

    [Fact]
    public async Task disconnect_cancels_a_launch_before_the_application_is_ready()
    {
        var launcher = new PendingLaunch();
        await using var client = await Client.StartAsync(launcher);
        await client.Initialize();
        var project = Path.GetFullPath("application");
        var launching = client.Send(new LaunchRequest
        {
            Args = { ConfigurationProperties = new Dictionary<string, JToken> { ["project"] = project } }
        });
        var options = await launcher.Started.Task.WaitAsync(client.Cancellation);
        Assert.Equal(new DebugLaunchOptions(project, "Debug", null), options);
        await client.Send(new DisconnectRequest { Args = { TerminateDebuggee = true } });
        await Assert.ThrowsAsync<ProtocolException>(() => launching);
        await launcher.Completed.Task.WaitAsync(client.Cancellation);
        Assert.False(client.Adapter.Failed);
    }

    [Theory]
    [InlineData("project", "\"relative/path\"")]
    [InlineData("configuration", "false")]
    [InlineData("watch", "1")]
    [InlineData("console", "\"unknown\"")]
    [InlineData("console", "\"integratedTerminal\"")]
    [InlineData("port", "-1")]
    [InlineData("port", "65536")]
    public async Task invalid_launch_options_do_not_start_an_application(string property, string json)
    {
        var launcher = new PendingLaunch();
        await using var client = await Client.StartAsync(launcher);
        await client.Initialize();
        var properties = new Dictionary<string, JToken> { ["project"] = Path.GetFullPath("application") };
        properties[property] = JToken.Parse(json);
        await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new LaunchRequest
        {
            Args = { ConfigurationProperties = properties }
        }));
        Assert.False(launcher.Started.Task.IsCompleted);
        Assert.False(client.Adapter.Failed);
    }

    [Fact]
    public async Task prepared_detachment_rejects_a_second_disconnect_while_committing()
    {
        await using var client = await Client.StartAsync();
        await client.Initialize();
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var attached = client.Send(Attach("127.0.0.1", ((IPEndPoint)listener.LocalEndpoint).Port));
        using var control = await AcceptControlAsync(listener, client.Cancellation);
        await client.Initialized.Task.WaitAsync(client.Cancellation);
        using var reader = new StreamReader(control.GetStream(), leaveOpen: true);
        using var writer = new StreamWriter(control.GetStream(), leaveOpen: true) { AutoFlush = true };
        var preparing = client.Send(new PrepareDetach());
        Assert.Equal("prepareDetach", JObject.Parse((await reader.ReadLineAsync(client.Cancellation))!)["action"]!.Value<string>());
        await writer.WriteLineAsync("{\"version\":2,\"action\":\"detachReady\"}");
        await preparing;
        var disconnecting = client.Send(new DisconnectRequest { Args = { TerminateDebuggee = false } });
        Assert.Equal("detach", JObject.Parse((await reader.ReadLineAsync(client.Cancellation))!)["action"]!.Value<string>());
        await Assert.ThrowsAsync<ProtocolException>(() => client.Send(new DisconnectRequest { Args = { TerminateDebuggee = false } }));
        await writer.WriteLineAsync("{\"version\":2,\"action\":\"detached\"}");
        await disconnecting;
        await Assert.ThrowsAsync<ProtocolException>(() => attached);
        Assert.False(client.Adapter.Failed);
    }

    private sealed class PrepareDetach() : DebugRequest<ConfigurationDoneArguments>("isoplane/prepareDetach");

    private sealed class PendingLaunch : DebugLauncher
    {
        internal TaskCompletionSource<DebugLaunchOptions> Started { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal TaskCompletionSource Completed { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public async Task RunAsync(DebugLaunchOptions options, Func<IPEndPoint, Task> ready, Action<string> output, Func<DebugTerminal, CancellationToken, Task>? terminal, CancellationToken cancellation)
        {
            Started.SetResult(options);
            try { await Task.Delay(Timeout.Infinite, cancellation); }
            finally { Completed.SetResult(); }
        }
    }

    /// <summary>Accepts the adapter's control connection as a local session whose application package is at <paramref name="package"/>.</summary>
    private static async Task<TcpClient> AcceptControlAsync(TcpListener listener, CancellationToken cancellation, string? package = null)
    {
        var control = await listener.AcceptTcpClientAsync(cancellation);
        using var writer = new StreamWriter(control.GetStream(), leaveOpen: true) { AutoFlush = true };
        await writer.WriteLineAsync(System.Text.Json.JsonSerializer.Serialize(new
        {
            version = 2, port = ((IPEndPoint)listener.LocalEndpoint).Port,
            package = package ?? Path.Combine(Path.GetTempPath(), "isoplane-test-no-application.tar")
        }));
        return control;
    }

    private static AttachRequest Attach(string address, int port) => new()
    {
        Args = { ConfigurationProperties = new Dictionary<string, JToken> { ["address"] = address, ["port"] = port } }
    };

    private sealed class Client : IAsyncDisposable
    {
        private readonly TcpClient editor;
        private readonly TcpClient adapter;
        private readonly Editor host;
        private DebugProtocolHost Protocol => host.Protocol;
        private readonly CancellationTokenSource lifetime = new(TimeSpan.FromSeconds(30));
        internal GuestDebugAdapter Adapter { get; }
        internal StringWriter Diagnostics { get; } = new();
        internal TaskCompletionSource Initialized { get; } = new(TaskCreationOptions.RunContinuationsAsynchronously);
        /// <summary>The breakpoints the adapter reported as changed, in order.</summary>
        internal System.Threading.Channels.Channel<Breakpoint> Breakpoints { get; } = System.Threading.Channels.Channel.CreateUnbounded<Breakpoint>();
        internal CancellationToken Cancellation => lifetime.Token;

        private Client(TcpClient editor, TcpClient adapter, DebugLauncher? launcher)
        {
            this.editor = editor;
            this.adapter = adapter;
            Adapter = new GuestDebugAdapter(adapter.GetStream(), adapter.GetStream(), Diagnostics, launcher);
            host = new Editor(editor.GetStream(), Initialized, Breakpoints.Writer, Diagnostics);
            Adapter.Protocol.Run();
            Protocol.LogMessage += (_, message) => Diagnostics.WriteLine(message.Category + ": " + message.Message);
            Adapter.Protocol.LogMessage += (_, message) => Diagnostics.WriteLine(message.Category + ": " + message.Message);
            Protocol.Run();
        }

        internal static async Task<Client> StartAsync(DebugLauncher? launcher = null)
        {
            using var listener = new TcpListener(IPAddress.Loopback, 0);
            listener.Start();
            var editor = new TcpClient();
            try
            {
                await editor.ConnectAsync((IPEndPoint)listener.LocalEndpoint);
                return new Client(editor, await listener.AcceptTcpClientAsync(), launcher);
            }
            catch { editor.Dispose(); throw; }
        }

        internal Task<InitializeResponse> Initialize() => Send(new InitializeRequest
        {
            Args = { AdapterID = "isoplane", LinesStartAt1 = true, ColumnsStartAt1 = true }
        });

        internal Task<TResponse> Send<TArguments, TResponse>(DebugRequestWithResponse<TArguments, TResponse> request)
            where TArguments : DebugRequestArguments, new() where TResponse : ResponseBody
        {
            var result = new TaskCompletionSource<TResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
            Protocol.SendRequest(request, (_, value) => result.TrySetResult(value), (_, error) => result.TrySetException(error), Cancellation);
            return Observe(result.Task);
        }

        internal Task Send<TArguments>(DebugRequest<TArguments> request) where TArguments : DebugRequestArguments, new()
        {
            var result = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            Protocol.SendRequest(request, _ => result.TrySetResult(), (_, error) => result.TrySetException(error), Cancellation);
            return Observe(result.Task);
        }

        private async Task<T> Observe<T>(Task<T> task)
        {
            try { return await task.WaitAsync(Cancellation); }
            catch (OperationCanceledException error) { throw new TimeoutException("DAP request did not complete: " + Diagnostics, error); }
        }

        private async Task Observe(Task task)
        {
            try { await task.WaitAsync(Cancellation); }
            catch (OperationCanceledException error) { throw new TimeoutException("DAP request did not complete: " + Diagnostics, error); }
        }

        /// <summary>Stops both protocol peers and releases the owned transport.</summary>
        public async ValueTask DisposeAsync()
        {
            editor.Dispose();
            adapter.Dispose();
            Protocol.Stop();
            await Adapter.DisposeAsync().AsTask().WaitAsync(Cancellation);
            await Task.Run(Protocol.WaitForReader).WaitAsync(TimeSpan.FromSeconds(30));
            await Task.Run(Adapter.Protocol.WaitForReader).WaitAsync(TimeSpan.FromSeconds(30));
            lifetime.Dispose();
        }
    }

    private sealed class Editor : DebugAdapterHostBase
    {
        private readonly TaskCompletionSource initialized;
        private readonly TextWriter diagnostics;

        private readonly System.Threading.Channels.ChannelWriter<Breakpoint> breakpoints;

        internal Editor(Stream stream, TaskCompletionSource initialized, System.Threading.Channels.ChannelWriter<Breakpoint> breakpoints, TextWriter diagnostics)
        {
            this.breakpoints = breakpoints;
            this.initialized = initialized;
            this.diagnostics = diagnostics;
            InitializeProtocolHost(stream, stream);
        }

        protected override void HandleProtocolError(Exception error) => diagnostics.WriteLine(error);

        protected override void HandleInitializedEvent(InitializedEvent message) => initialized.TrySetResult();
        protected override void HandleBreakpointEvent(BreakpointEvent message) => breakpoints.TryWrite(message.Breakpoint);
        protected override void HandleTerminatedEvent(TerminatedEvent message) => diagnostics.WriteLine("The debugger session terminated.");
    }
}
