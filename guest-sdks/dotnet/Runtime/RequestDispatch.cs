using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane.Runtime;

internal static class RequestDispatch
{
    internal static async Task<Response> RouteAsync(IIsoplaneRouter router, DispatchRecord record)
    {
        // A guest the host did not pre-initialize initializes before its first dispatch,
        // so an application's initializer has always run by the time it handles a request.
        await Initialization.EnterAsync();
        await Activation.EnterAsync();
        if (record.Kind != DispatchKind.Http) return await router.Route(record);
        bool hostCanHaveBody = H.ReqBodyCanHaveBody() != 0;
        bool canHaveBody = hostCanHaveBody || record.Body.Length != 0;
        using var body = new RequestStream(record.Body, hostCanHaveBody);
        if (router is IStreamingHttpRouter streaming && streaming.StreamsRequest(new RequestHead(record)))
            return await streaming.RouteStreamingAsync(new StreamingRequest(record, body, canHaveBody));
        using (var buffered = new System.IO.MemoryStream())
        {
            await body.CopyToAsync(buffered);
            record.Body = buffered.ToArray();
        }
        return await router.Route(record);
    }

}
