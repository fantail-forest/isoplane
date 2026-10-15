using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Exercises ordinary managed deadlines across host parking.</summary>
public static class TimerProbe
{
    private static int _abandonedCallbacks;
    private static int _requests;

    /// <summary>Exercises awaited timers and observes abandoned timer continuations.</summary>
    public static async Task<Response> HandleAsync(string path)
    {
        _requests++;
        if (path == "/abandon")
        {
            _ = AbandonedDelay();
            return Response.Text("armed");
        }
        if (path == "/observe")
            return Response.Text($"{_requests}:{_abandonedCallbacks}");
        await Task.Delay(100).ConfigureAwait(false);
        return Response.Text("timer resumed");
    }
    private static async Task AbandonedDelay()
    {
        await Task.Delay(100).ConfigureAwait(false);
        _abandonedCallbacks++;
    }
}
