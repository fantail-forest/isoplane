#if WISP_STREAMING_PROBE
using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

public sealed class StreamingProbe : IWispHandler
{
    public async Task<Response> HandleAsync(Request request, Ctx context)
    {
        if (request.Path == "/pending")
        {
            var stream = Response.Stream(200, Array.Empty<KeyValuePair<string, string>>());
            var pending = stream.WriteAsync(new byte[] { 1 });
            int refusals = 0;
            try { await stream.WriteAsync(new byte[] { 2 }); }
            catch (InvalidOperationException) { refusals++; }
            try { stream.Complete(); }
            catch (InvalidOperationException) { refusals++; }
            if (refusals != 2) throw new InvalidOperationException("Pending response writes must prevent concurrent writes and completion.");
            await pending;
            return stream.Complete();
        }
        try { Response.Stream(200, Array.Empty<KeyValuePair<string, string>>()); }
        catch (InvalidOperationException)
        {
            return Response.Text("creation-rejected");
        }
        throw new InvalidOperationException("A refused response handle escaped the SDK guard.");
    }
}
#endif
