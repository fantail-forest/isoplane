using System.Threading.Tasks;
using Wisp.Abi;

namespace Wisp.Durable;

/// <summary>
/// What routes a decoded dispatch to guest code.
///
/// This is the seam that lets one SDK serve many guests. <see cref="Exports{T}"/>
/// holds every line of the five host exports — the allocation handshake, the
/// poll-status packing, the trap conversion, the future table — and knows
/// nothing about any particular guest. A guest supplies one of these, and the
/// SDK never has to name a tenant type.
///
/// It is the C# analogue of the Rust SDK's <c>export_handler!</c> /
/// <c>export_durable!</c>: the same one-line declaration of "this class is the
/// entrypoint", expressed as a generic argument because there is no macro.
/// </summary>
public interface IWispRouter
{
    /// <summary>Called once per wasm instance, before the first dispatch.</summary>
    void Boot();

    /// <summary>
    /// Route one decoded dispatch. A trigger this guest cannot receive must
    /// throw <see cref="PostcardException"/>, which the entrypoint turns into
    /// <see cref="TrapCode.Protocol"/> — §13: the dispatch fails and the
    /// instance is discarded, rather than answering wrongly.
    /// </summary>
    Task<Response> Route(DispatchRecord d, Ctx ctx);
}

/// <summary>
/// A §2.1 Handler: HTTP in, response out, instance drawn from a pool.
/// </summary>
public interface IWispHandler
{
    Task<Response> HandleAsync(Request req, Ctx ctx);
}

/// <summary>
/// Routes to a Handler. A Handler may only ever receive
/// <see cref="DispatchKind.Http"/>; every other trigger is a host misrouting.
/// </summary>
public sealed class HandlerRouter<TApp> : IWispRouter
    where TApp : IWispHandler, new()
{
    private TApp? _app;

    public void Boot() => _app = new TApp();

    public Task<Response> Route(DispatchRecord d, Ctx ctx)
    {
        if (d.Kind != DispatchKind.Http) throw new PostcardException();
        return _app!.HandleAsync(new Request(d.Method, d.Path), ctx);
    }
}
