namespace Isoplane.Cli;

internal static class Development
{
    internal static async Task<int> RunAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        if (!options.ShutdownOnInputClose)
            return await RunOwnedAsync(options, cancellation);
        using var lifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        var inputCancellation = lifetime.Token;
        using var standardInput = Console.OpenStandardInput();
        var input = Task.Run(() => WaitForInputCloseAsync(standardInput, inputCancellation));
        var session = RunOwnedAsync(options, lifetime.Token);
        try
        {
            if (await Task.WhenAny(session, input) == input)
            {
                await input;
                lifetime.Cancel();
            }
            return await session;
        }
        finally
        {
            // A console read can remain blocked until process exit; it owns no child work.
            lifetime.Cancel();
        }
    }

    private static async Task WaitForInputCloseAsync(Stream standardInput, CancellationToken cancellation)
    {
        var input = new byte[1];
        while (await standardInput.ReadAsync(input.AsMemory(), cancellation) != 0) { }
    }

    private static async Task<int> RunOwnedAsync(DevelopmentOptions options, CancellationToken cancellation)
    {
        if (File.Exists(Path.Combine(options.Directory, "isoplane.toml")))
            Console.Error.WriteLine("isoplane: isoplane.toml is no longer read; delete it. Development settings come from Properties/launchSettings.json and .NET User Secrets.");
        if (options.Debug)
            return await DebugDevelopment.RunAsync(options, cancellation);
        await using var session = await DevelopmentSession.StartAsync(options, message => LocalEvents.Write(message, options.Json), cancellation);
        await session.Completion;
        return 0;
    }
}
