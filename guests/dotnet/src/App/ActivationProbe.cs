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
    private static int _initializations;
    private static string _preparedSetting = "";

    static ActivationProbe()
    {
        Initialization.Initializing += async () =>
        {
            await Task.Yield();
            _initializations++;
            _preparedSetting = RuntimeConfiguration.ReadValues()["probe"];
        };
        Activation.Activated += async () =>
        {
            await Task.Yield();
            if (RuntimeConfiguration.ReadValues().ContainsKey("activation-timer"))
                await Task.Delay(1);
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
            case "/prepared-setting":
                return Task.FromResult(Response.Text(_preparedSetting));
            case "/initializations":
                return Task.FromResult(Response.Text(_initializations.ToString()));
            case "/activations":
                return Task.FromResult(Response.Text(_activations.ToString()));
            default:
                throw new ArgumentException("Unknown activation probe path.", nameof(request));
        }
    }
}
#endif
