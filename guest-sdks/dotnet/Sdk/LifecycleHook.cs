using System;
using System.Threading.Tasks;

namespace Isoplane;

internal sealed class LifecycleHook
{
    private Task? completion;

    internal bool IsCompleted => completion?.IsCompletedSuccessfully == true;

    internal Task EnterAsync(Func<Task>? callbacks, Func<Task>? completing = null) => completion ??= InvokeAsync(callbacks, completing);

    private static async Task InvokeAsync(Func<Task>? callbacks, Func<Task>? completing)
    {
        await InvokeCallbacksAsync(callbacks);
        await InvokeCallbacksAsync(completing);
    }

    internal static async Task InvokeCallbacksAsync(Func<Task>? callbacks)
    {
        if (callbacks is null) return;
        foreach (Func<Task> callback in callbacks.GetInvocationList())
            await callback();
    }
}
