namespace Isoplane.Cli;

internal static class SourceReload
{
    internal static async Task RunAsync(SourceChanges changes, Task exited, Func<CancellationToken, Task> reload, CancellationToken cancellation)
    {
        while (true)
        {
            using var watchLifetime = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
            var changed = changes.WaitAsync(watchLifetime.Token);
            if (await Task.WhenAny(exited, changed) == exited)
            {
                watchLifetime.Cancel();
                try { await changed; }
                catch (OperationCanceledException) when (watchLifetime.IsCancellationRequested)
                {
                    // The exited runtime no longer needs discovery, but its process cleanup still has to finish.
                }
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
