using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

/// <summary>Exercises ordinary managed deadlines across host parking.</summary>
public sealed class TimerProbe : IWispHandler
{
    private int _abandonedCallbacks;
    private int _requests;

    /// <summary>Exercises awaited timers and observes abandoned timer continuations.</summary>
    public async Task<Response> HandleAsync(Request request, Ctx context)
    {
        _requests++;
        if (request.Path == "/abandon")
        {
            _ = AbandonedDelay();
            return Response.Text("armed");
        }
        if (request.Path == "/observe")
            return Response.Text($"{_requests}:{_abandonedCallbacks}");
        await Task.Delay(100).ConfigureAwait(false);
        return Response.Text("timer resumed");
    }
    private async Task AbandonedDelay()
    {
        await Task.Delay(100).ConfigureAwait(false);
        _abandonedCallbacks++;
    }
}
