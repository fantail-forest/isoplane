using System.Threading.Tasks;
using Wisp.Abi;

namespace Wisp.Durable;

/// <summary>
/// What routes a decoded dispatch to guest code. <see cref="Exports{T}"/>
/// is generic over one of these, so the SDK names no tenant type.
/// </summary>
public interface IWispRouter
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

/// <summary>
/// A Handler: an HTTP request in, a response out.
/// </summary>
public interface IWispHandler
{
    /// <summary>
    /// Handle one HTTP request. Whether the instance is reused between
    /// requests or reset first is the app's <c>lifecycle</c> setting, and a
    /// handler depends on neither.
    /// </summary>
    Task<Response> HandleAsync(Request req, Ctx ctx);
}

/// <summary>
/// Routes HTTP dispatches to a Handler; any other trigger is a
/// <see cref="PostcardException"/>.
/// </summary>
public sealed class HandlerRouter<TApp> : IWispRouter
    where TApp : IWispHandler, new()
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
