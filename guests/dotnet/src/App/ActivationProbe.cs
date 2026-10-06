#if ISOPLANE_ACTIVATION_PROBE
using System;
using System.Security.Cryptography;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>
/// A generator seeded once during initialization, before the template snapshot, and
/// reseeded on activation: the activation hook is what keeps two copies of
/// the template from drawing the same sequence.
/// </summary>
internal static class ActivationProbe
{
    private static Random _seeded = Fresh();
    private static int _activations;
    private static int _initializations;
    private static string _preparedSetting = "";

    [Initializer]
    public static async Task InitializeAsync()
    {
        await Task.Yield();
        _initializations++;
        _preparedSetting = RuntimeConfiguration.ReadValues()["probe"];
    }

    [Activated]
    public static async Task ActivatedAsync()
    {
        await Task.Yield();
        if (RuntimeConfiguration.ReadValues().ContainsKey("activation-timer"))
            await Task.Delay(1);
        _activations++;
        _seeded = Fresh();
    }

    private static Random Fresh() => new(RandomNumberGenerator.GetInt32(int.MaxValue));

    [HttpHandler]
    public static Task<Response> HandleAsync(Request request, Ctx context)
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
