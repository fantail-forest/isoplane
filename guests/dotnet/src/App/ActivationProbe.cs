#if WISP_ACTIVATION_PROBE
using System;
using System.Security.Cryptography;
using System.Threading.Tasks;
using Wisp.Durable;

namespace Wisp.App;

/// <summary>
/// A generator seeded once at boot, before the template snapshot, and
/// reseeded on activation: the activation hook is what keeps two copies of
/// the template from drawing the same sequence.
/// </summary>
internal sealed class ActivationProbe : IWispHandler
{
    private static Random _seeded = Fresh();
    private static int _activations;

    static ActivationProbe()
    {
        Activation.Activated += () =>
        {
            _activations++;
            _seeded = Fresh();
        };
    }

    private static Random Fresh() => new(RandomNumberGenerator.GetInt32(int.MaxValue));

    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        switch (request.Path)
        {
            case "/draw":
                return Task.FromResult(Response.Text(_seeded.Next().ToString("X8")));
            case "/activations":
                return Task.FromResult(Response.Text(_activations.ToString()));
            default:
                throw new ArgumentException("Unknown activation probe path.", nameof(request));
        }
    }
}
#endif
