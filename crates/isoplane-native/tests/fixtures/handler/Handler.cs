using Isoplane;

public static class Handler
{
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request request)
    {
        if (request.Path != "/async") return Response.Text("Hello from Isoplane!");
        await Task.Delay(1);
        return Response.Text("async-ready");
    }
}
