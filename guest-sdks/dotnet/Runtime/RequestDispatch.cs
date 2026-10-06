using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane.Runtime;

internal static class RequestDispatch
{
    internal static async Task<Response> RouteAsync(Isoplane.IIsoplaneRouter router, DispatchRecord record, Ctx context)
    {
        // A guest the host did not pre-initialize initializes before its first dispatch,
        // so an application's initializer has always run by the time it handles a request.
        await Initialization.EnterAsync();
        await Activation.EnterAsync();
        if (record.Kind != DispatchKind.Http) return await router.Route(record, context);
        bool hostCanHaveBody = H.ReqBodyCanHaveBody() != 0;
        bool canHaveBody = hostCanHaveBody || record.Body.Length != 0;
        using var body = new RequestStream(record.Body, hostCanHaveBody);
        if (router is Isoplane.IStreamingHttpRouter streaming && streaming.StreamsRequest(new RequestHead(record)))
            return await streaming.RouteStreamingAsync(new StreamingRequest(record, body, canHaveBody), context);
        using (var buffered = new System.IO.MemoryStream())
        {
            await body.CopyToAsync(buffered);
            record.Body = buffered.ToArray();
        }
        return await router.Route(record, context);
    }

}
