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
    private readonly Func<Request, Ctx, Task<Response>>? buffered;
    private readonly Func<StreamingRequest, Ctx, Task<Response>>? streaming;
    private readonly Func<Task>? initializer;
    private readonly Func<Task>? activated;

    /// <summary>Binds the methods the build recorded. Throws <see cref="ArgumentException"/> when one has another signature.</summary>
    internal AttributedHandlerRouter(MethodInfo http, MethodInfo? initializer, MethodInfo? activated)
    {
        ParameterInfo[] parameters = http.GetParameters();
        bool context = parameters.Length == 2;
        if (parameters.Length > 0 && parameters[0].ParameterType == typeof(StreamingRequest))
            streaming = context ? http.CreateDelegate<Func<StreamingRequest, Ctx, Task<Response>>>()
                : Discard(http.CreateDelegate<Func<StreamingRequest, Task<Response>>>());
        else
            buffered = context ? http.CreateDelegate<Func<Request, Ctx, Task<Response>>>()
                : Discard(http.CreateDelegate<Func<Request, Task<Response>>>());
        this.initializer = initializer?.CreateDelegate<Func<Task>>();
        this.activated = activated?.CreateDelegate<Func<Task>>();
    }

    private static Func<T, Ctx, Task<Response>> Discard<T>(Func<T, Task<Response>> handler) => (request, _) => handler(request);

    public void Boot()
    {
        if (initializer is not null) Initialization.Initializing += initializer;
        if (activated is not null) Activation.Activated += activated;
    }

    public bool StreamsRequest(RequestHead request) => streaming is not null;

    public Task<Response> RouteStreamingAsync(StreamingRequest request, Ctx ctx) => streaming!(request, ctx);

    public Task<Response> Route(DispatchRecord d, Ctx ctx)
    {
        if (d.Kind != DispatchKind.Http || buffered is null) throw new PostcardException();
        return buffered(Request.FromDispatch(d), ctx);
    }
}
