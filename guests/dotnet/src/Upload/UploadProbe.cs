using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>Reads an upload through a handler that takes the request body as a stream.</summary>
public static class UploadProbe
{
    private static string _leftRead = "none";

    /// <summary>
    /// Returns the uploaded bytes behind a marker that only this handler writes. <c>/leave-read</c>
    /// instead responds while a read of the upload is still pending, and <c>/left-read</c> reports
    /// how that read ended.
    /// </summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(StreamingRequest request)
    {
        if (request.Path is "/cancel-read" or "/cancel-then-read")
        {
            using var cancellation = new CancellationTokenSource();
            var read = request.Body.ReadAsync(new byte[16], cancellation.Token);
            cancellation.Cancel();
            try
            {
                await read;
                return Response.Text("read was not cancelled");
            }
            catch (OperationCanceledException)
            {
                if (request.Path == "/cancel-read") return Response.Text("cancelled");
                using var resumed = new MemoryStream();
                byte[] buffer = new byte[3];
                int count;
                while ((count = await request.Body.ReadAsync(buffer)) != 0) resumed.Write(buffer, 0, count);
                return Response.Text("resumed:" + Convert.ToBase64String(resumed.ToArray()));
            }
        }
        if (request.Path == "/leave-read")
        {
            _leftRead = "pending";
            _ = ReadAfterResponseAsync(request.Body);
            return Response.Text("left");
        }
        if (request.Path == "/left-read") return Response.Text(_leftRead);
        using var bytes = new MemoryStream();
        await request.Body.CopyToAsync(bytes);
        return Response.Text("stream:" + Convert.ToBase64String(bytes.ToArray()));
    }

    private static async Task ReadAfterResponseAsync(Stream body)
    {
        try
        {
            _leftRead = "read " + await body.ReadAsync(new byte[16]);
        }
        catch (IOException)
        {
            _leftRead = "failed";
        }
    }
}
