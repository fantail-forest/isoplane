using Isoplane;

public static class Handler
{
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request)
    {
        Console.WriteLine("managed-session-guest-log");
        return Task.FromResult(Response.Text("working publication"));
    }
}
