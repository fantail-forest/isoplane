using System;
using System.Linq;
using System.Reflection;
using Isoplane;

namespace Isoplane.Probes;

/// <summary>Constructs the one handler compiled into this probe package.</summary>
public static class Factory
{
    /// <summary>Creates the probe when the shared runtime initializes its application.</summary>
    public static IIsoplaneHandler Create()
    {
        var handler = Assembly.GetExecutingAssembly().DefinedTypes.Single(type =>
            !type.IsAbstract && typeof(IIsoplaneHandler).IsAssignableFrom(type));
        return (IIsoplaneHandler)(Activator.CreateInstance(handler.AsType(), nonPublic: true)
            ?? throw new InvalidOperationException("The probe constructor returned no handler."));
    }
}
