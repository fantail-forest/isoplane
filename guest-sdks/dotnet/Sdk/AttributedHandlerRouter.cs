using System;
using System.Reflection;
using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane;

/// <summary>
/// Routes HTTP dispatches to an application's <see cref="HttpHandlerAttribute"/> method and
/// subscribes its lifecycle methods. Any other trigger is a <see cref="PostcardException"/>.
/// </summary>
internal sealed class AttributedHandlerRouter : IStreamingHttpRouter
{
    private readonly Func<Request, Task<Response>>? buffered;
    private readonly Func<StreamingRequest, Task<Response>>? streaming;
    private readonly Func<Task>? initializer;
    private readonly Func<Task>? activated;

    /// <summary>Binds the methods the build recorded. Throws <see cref="ArgumentException"/> when one has another signature.</summary>
    internal AttributedHandlerRouter(MethodInfo http, MethodInfo? initializer, MethodInfo? activated)
    {
        ParameterInfo[] parameters = http.GetParameters();
        if (parameters.Length > 0 && parameters[0].ParameterType == typeof(StreamingRequest))
            streaming = http.CreateDelegate<Func<StreamingRequest, Task<Response>>>();
        else
            buffered = http.CreateDelegate<Func<Request, Task<Response>>>();
        this.initializer = initializer?.CreateDelegate<Func<Task>>();
        this.activated = activated?.CreateDelegate<Func<Task>>();
    }

    public void Boot()
    {
        if (initializer is not null) Initialization.Initializing += initializer;
        if (activated is not null) Activation.Activated += activated;
    }

    public bool StreamsRequest(RequestHead request) => streaming is not null;

    public Task<Response> RouteStreamingAsync(StreamingRequest request) => streaming!(request);

    public Task<Response> Route(DispatchRecord d)
    {
        if (d.Kind != DispatchKind.Http || buffered is null) throw new PostcardException();
        return buffered(Request.FromDispatch(d));
    }
}
