using System;
using System.IO;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Reads an upload through a handler that takes the request body as a stream.</summary>
public static class UploadProbe
{
    /// <summary>Returns the uploaded bytes behind a marker that only this handler writes.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(StreamingRequest request)
    {
        using var bytes = new MemoryStream();
        await request.Body.CopyToAsync(bytes);
        return Response.Text("stream:" + Convert.ToBase64String(bytes.ToArray()));
    }
}
