using System;
using System.Threading.Tasks;

namespace Isoplane;

internal sealed class LifecycleHook
{
    private Task? completion;

    internal bool IsCompleted => completion?.IsCompletedSuccessfully == true;

    internal Task EnterAsync(Func<Task>? callbacks) => completion ??= InvokeAsync(callbacks);

    private static async Task InvokeAsync(Func<Task>? callbacks)
    {
        if (callbacks is null) return;
        foreach (Func<Task> callback in callbacks.GetInvocationList())
            await callback();
    }
}
