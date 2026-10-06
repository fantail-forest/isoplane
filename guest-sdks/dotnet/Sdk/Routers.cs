using System.Threading.Tasks;
using Isoplane.Abi;

namespace Isoplane;

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
