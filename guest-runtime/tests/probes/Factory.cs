using System;
using System.Linq;
using System.Reflection;
using Isoplane;

namespace Isoplane.Probes;

/// <summary>Constructs the application compiled into this probe package.</summary>
public static class Factory
{
    /// <summary>Creates the probe when the shared runtime initializes its application.</summary>
#if ISOPLANE_REQUEST_PROBE
    public static IIsoplaneRouter Create() => new Isoplane.App.RequestProbeRouter();
#else
    public static IIsoplaneHandler Create()
    {
        var handler = Assembly.GetExecutingAssembly().DefinedTypes.Single(type =>
            !type.IsAbstract && typeof(IIsoplaneHandler).IsAssignableFrom(type));
        return (IIsoplaneHandler)(Activator.CreateInstance(handler.AsType(), nonPublic: true)
            ?? throw new InvalidOperationException("The probe constructor returned no handler."));
    }
#endif
}
