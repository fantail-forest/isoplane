using System.Threading.Tasks;

using Isoplane;

namespace Isoplane.App;

/// <summary>
/// The tenant's handler, written the way a handler is meant to be written: a
/// plain <c>async Task&lt;Response&gt;</c> method with an <c>await</c> in the
/// middle of it. No yield, no callback, no dialect.
///
/// The point of this file is that there is nothing interesting in it. The goal
/// is "C# as-is, not a transpiler"; the evidence for that is that the guest's
/// business logic is indistinguishable from ordinary .NET while the platform
/// still gets an empty stack at the await (invariant 3). Everything that
/// makes that true lives in <c>Runtime/</c> and none of it is visible here.
/// </summary>
public static class Api
{
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request req)
    {
        await Task.Delay(1);
        return Response.Text(req.Path);
    }
}
