using System;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;

namespace Debugger.Uncaught;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/uncaught", Handle);

    private static async Task<string> Handle(HttpRequest request)
    {
        int caught = 0;
        try { throw new InvalidOperationException("debug-caught"); }
        catch (InvalidOperationException) { caught++; }
        if (!request.Query.ContainsKey("fatal"))
        {
            await Task.Yield();
            throw new InvalidOperationException("debug-request-failed");
        }
        FailWithoutTask();
        await new TaskCompletionSource<string>().Task;
        return caught.ToString();
    }

    private static async void FailWithoutTask()
    {
        await Task.Yield();
        throw new InvalidOperationException("debug-unhandled");
    }
}
