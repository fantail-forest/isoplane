using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

/// <summary>Exercises ordinary managed deadlines across host parking.</summary>
public sealed class TimerProbe : IWispHandler
{
    /// <summary>Returns only after the managed timer continuation executes.</summary>
    public async Task<Response> HandleAsync(Request request, Ctx context)
    {
        await Task.Delay(100).ConfigureAwait(false);
        return Response.Text("timer resumed");
    }
}
