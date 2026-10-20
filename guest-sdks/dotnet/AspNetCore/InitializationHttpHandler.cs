namespace Isoplane.AspNetCore;

internal sealed class InitializationHttpHandler : DelegatingHandler
{
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        // A live initialization connection cannot be captured in the template.
        if (!Initialization.IsCompleted) request.Headers.ConnectionClose = true;
        return base.SendAsync(request, cancellationToken);
    }
}
