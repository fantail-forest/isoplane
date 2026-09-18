using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane.Durable;

/// <summary>
/// What routes a decoded dispatch to guest code. <see cref="Exports{T}"/>
/// is generic over one of these, so the SDK names no tenant type.
/// </summary>
public interface IIsoplaneRouter
{
    /// <summary>Called once per wasm instance, before the first dispatch.</summary>
    void Boot();

    /// <summary>
    /// Route one decoded dispatch. A trigger this guest cannot receive throws
    /// <see cref="PostcardException"/>, which the entrypoint turns into
    /// <see cref="TrapCode.Protocol"/>.
    /// </summary>
    Task<Response> Route(DispatchRecord d, Ctx ctx);
}

/// <summary>Explicitly selects HTTP routes that consume uploads without SDK buffering.</summary>
public interface IStreamingHttpRouter : IIsoplaneRouter
{
    /// <summary>Returns true only for requests handled by RouteStreamingAsync; other routes receive full buffered bodies.</summary>
    bool StreamsRequest(RequestHead request);

    /// <summary>Handles a selected HTTP request while its body can still arrive.</summary>
    Task<Response> RouteStreamingAsync(StreamingRequest request, Ctx ctx);
}

/// <summary>
/// A Handler: an HTTP request in, a response out.
/// </summary>
public interface IIsoplaneHandler
{
    /// <summary>
    /// Handles one HTTP request. Instances may be reused or evicted between requests;
    /// do not rely on retained in-process state or work continuing after the response.
    /// </summary>
    Task<Response> HandleAsync(Request req, Ctx ctx);
}

/// <summary>
/// Routes HTTP dispatches to a Handler; any other trigger is a
/// <see cref="PostcardException"/>.
/// </summary>
public sealed class HandlerRouter<TApp> : IIsoplaneRouter
    where TApp : IIsoplaneHandler, new()
{
    private TApp? _app;

    /// <summary>
    /// Constructs the app once per wasm instance, before the first dispatch.
    /// </summary>
    public void Boot() => _app = new TApp();

    /// <summary>
    /// Routes an HTTP dispatch to the app.
    /// </summary>
    public Task<Response> Route(DispatchRecord d, Ctx ctx)
    {
        if (d.Kind != DispatchKind.Http) throw new PostcardException();
        return _app!.HandleAsync(new Request(d), ctx);
    }
}
