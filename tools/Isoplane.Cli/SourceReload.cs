namespace Isoplane.Cli;

internal static class SourceReload
{
    internal static async Task RunAsync(SourceChanges changes, Task exited, Func<CancellationToken, Task> reload, CancellationToken cancellation)
    {
        while (true)
        {
            var changed = changes.WaitAsync(cancellation);
            if (await Task.WhenAny(exited, changed) == exited)
            {
                await exited;
                return;
            }
            await changed;
            using var buildLifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            var rebuild = reload(buildLifetime.Token);
            if (await Task.WhenAny(rebuild, exited) == exited)
            {
                buildLifetime.Cancel();
                try { await rebuild; }
                catch (OperationCanceledException) when (buildLifetime.IsCancellationRequested)
                {
                    Console.Error.WriteLine("isoplane: cancelled publication because the runtime exited.");
                }
                await exited;
                return;
            }
            await rebuild;
        }
    }
}
