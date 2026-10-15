using System;
using System.Diagnostics;
using System.Security.Cryptography;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>
/// A generator seeded once during initialization, before the template snapshot, and
/// reseeded on activation: the activation hook is what keeps two copies of
/// the template from drawing the same sequence. That generator was given a
/// seed, so only the app can reseed it. The generators that seed themselves
/// are also created or drawn from during initialization, as a warm-up request
/// would, and are left for the runtime to reseed. One with a fixed seed is
/// there to show that the runtime leaves a deterministic generator alone.
/// </summary>
internal static class ActivationProbe
{
    private static Random _seeded = Fresh();
    private static readonly Random Unseeded = new();
    private static readonly Random UnseededDerived = new Derived();
    private static readonly Random FixedSeed = new(42);
    private static int _activations;
    private static int _initializations;
    private static string _preparedSetting = "";

    [Initializer]
    public static async Task InitializeAsync()
    {
        await Task.Yield();
        _initializations++;
        _preparedSetting = RuntimeConfiguration.ReadValues()["probe"];
        _ = Random.Shared.Next();
        _ = ActivityTraceId.CreateRandom();
        _ = Unseeded.Next();
        _ = UnseededDerived.Next();
        _ = FixedSeed.Next();
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

    private sealed class Derived : Random;

    [HttpHandler]
    public static Task<Response> HandleAsync(Request request)
    {
        switch (request.Path)
        {
            case "/draw":
                return Task.FromResult(Response.Text(_seeded.Next().ToString("X8")));
            case "/shared-draw":
                return Task.FromResult(Response.Text(Random.Shared.Next().ToString("X8")));
            case "/trace-id":
                return Task.FromResult(Response.Text(ActivityTraceId.CreateRandom().ToHexString()));
            case "/unseeded-draw":
                return Task.FromResult(Response.Text(Unseeded.Next().ToString("X8")));
            case "/unseeded-derived-draw":
                return Task.FromResult(Response.Text(UnseededDerived.Next().ToString("X8")));
            case "/fixed-seed-draw":
                return Task.FromResult(Response.Text(FixedSeed.Next().ToString("X8")));
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
