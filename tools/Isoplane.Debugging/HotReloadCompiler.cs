using System.Diagnostics;
using System.Text.Json;
using System.Text;
using Isoplane.Build;

namespace Isoplane.Debugging;

internal sealed class HotReloadCompiler : IAsyncDisposable
{
    private readonly Process process;
    private readonly Task<string> diagnostics;
    private readonly SemaphoreSlim commands = new(1, 1);
    private bool disposed;

    internal string[] Documents { get; private set; } = [];

    private HotReloadCompiler(Process process)
    {
        this.process = process;
        diagnostics = process.StandardError.ReadToEndAsync();
    }

    internal static async Task<HotReloadCompiler> StartAsync(string project, Dictionary<string, string> properties,
        Dictionary<string, string> assemblies, string[] capabilities, CancellationToken cancellation)
    {
        var query = new ProcessStartInfo("dotnet")
        {
            RedirectStandardOutput = true, RedirectStandardError = true, UseShellExecute = false,
            WorkingDirectory = Path.GetDirectoryName(project)!
        };
        foreach (string argument in new[] { "msbuild", project, "-nologo", "-nodeReuse:false", "-getProperty:MSBuildBinPath,AssemblyName" })
            query.ArgumentList.Add(argument);
        foreach (var property in properties) query.ArgumentList.Add("-property:" + property.Key + "=" + property.Value);
        using var process = Process.Start(query) ?? throw new IOException("Could not locate the project's .NET SDK.");
        using var cancelled = cancellation.Register(() => Kill(process));
        var output = process.StandardOutput.ReadToEndAsync(cancellation);
        var errors = process.StandardError.ReadToEndAsync(cancellation);
        await process.WaitForExitAsync(cancellation);
        string diagnostics = await errors;
        string result = await output;
        if (process.ExitCode != 0) throw new IOException($"Could not query the project's .NET SDK: {diagnostics} {result}");
        if (!string.IsNullOrWhiteSpace(diagnostics)) await Console.Error.WriteLineAsync(diagnostics);
        using var metadata = JsonDocument.Parse(result);
        var values = metadata.RootElement.GetProperty("Properties");
        string sdk = values.GetProperty("MSBuildBinPath").GetString() ?? throw new IOException("MSBuild did not identify its SDK directory.");
        string name = values.GetProperty("AssemblyName").GetString() ?? throw new IOException("MSBuild did not identify its entry assembly.");
        if (!assemblies.ContainsKey(name)) throw new IOException($"The paused application has no baseline for assembly '{name}'.");
        return await StartAsync(new CompilerStart(sdk, project, properties, assemblies, capabilities), cancellation);
    }

    internal static async Task<HotReloadCompiler> StartAsync(CompilerStart start, CancellationToken cancellation)
    {
        string path = Path.Combine(AppContext.BaseDirectory, "hotreload", "Isoplane.HotReload.dll");
        if (!File.Exists(path)) throw new IOException("The installed debugger is missing its Hot Reload compiler.");
        var options = new ProcessStartInfo("dotnet")
        {
            RedirectStandardInput = true, RedirectStandardOutput = true, RedirectStandardError = true,
            StandardInputEncoding = new UTF8Encoding(false), StandardOutputEncoding = new UTF8Encoding(false),
            StandardErrorEncoding = new UTF8Encoding(false),
            UseShellExecute = false, WorkingDirectory = Path.GetDirectoryName(start.Project)!
        };
        options.ArgumentList.Add(path);
        var process = Process.Start(options) ?? throw new IOException("The Hot Reload compiler could not be started.");
        var compiler = new HotReloadCompiler(process);
        try
        {
            using var cancelled = cancellation.Register(compiler.Kill);
            await HotReloadProtocol.WriteAsync(process.StandardInput, start);
            compiler.Documents = (await compiler.ReadAsync(cancellation)).Documents;
            return compiler;
        }
        catch
        {
            await compiler.DisposeAsync();
            throw;
        }
    }

    internal async Task<CompilerReply> SendAsync(CompilerCommand command, CancellationToken cancellation)
    {
        await commands.WaitAsync(cancellation);
        try
        {
            ObjectDisposedException.ThrowIf(disposed, this);
            using var cancelled = cancellation.Register(Kill);
            await HotReloadProtocol.WriteAsync(process.StandardInput, command);
            return await ReadAsync(cancellation);
        }
        finally { commands.Release(); }
    }

    private async Task<CompilerReply> ReadAsync(CancellationToken cancellation)
    {
        var reply = await HotReloadProtocol.ReadAsync<CompilerReply>(process.StandardOutput).WaitAsync(cancellation);
        if (reply is null)
        {
            await process.WaitForExitAsync(cancellation);
            throw new IOException($"The Hot Reload compiler exited with code {process.ExitCode}: {await diagnostics}");
        }
        if (reply.Error is { } error)
        {
            await process.WaitForExitAsync(cancellation);
            throw new IOException($"The Hot Reload compiler failed: {error}{Environment.NewLine}{await diagnostics}");
        }
        return reply;
    }

    private void Kill() => Kill(process);

    private static void Kill(Process process)
    {
        try { if (!process.HasExited) process.Kill(entireProcessTree: true); }
        catch (InvalidOperationException) { /* The process can exit between the check and termination. */ }
    }

    public async ValueTask DisposeAsync()
    {
        await commands.WaitAsync();
        try
        {
            if (disposed) return;
            disposed = true;
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(10));
            using var cancelled = deadline.Token.Register(Kill);
            try
            {
                if (!process.HasExited)
                {
                    await HotReloadProtocol.WriteAsync(process.StandardInput, new CompilerCommand("stop"));
                    process.StandardInput.Close();
                }
            }
            catch (IOException error)
            {
                await Console.Error.WriteLineAsync($"isoplane: Hot Reload compiler closed during shutdown: {error.Message}");
                Kill();
            }
            await process.WaitForExitAsync();
            string output = await diagnostics;
            if (!string.IsNullOrWhiteSpace(output)) await Console.Error.WriteLineAsync(output);
            process.Dispose();
        }
        finally { commands.Release(); }
    }
}
