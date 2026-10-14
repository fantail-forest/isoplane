using Isoplane;

public static class Handler
{
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) => Task.FromResult(Response.Text("Hello from Isoplane!"));
}
