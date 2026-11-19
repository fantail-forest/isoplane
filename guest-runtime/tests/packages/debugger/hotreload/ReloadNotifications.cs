#nullable enable
using System;
using System.Reflection.Metadata;

[assembly: MetadataUpdateHandler(typeof(Debugger.HotReload.ReloadNotifications))]

namespace Debugger.HotReload;

internal static class ReloadNotifications
{
    internal static bool Enabled;
    internal static int Clears;
    internal static int Updates;
    internal static bool TypesMatched;

    public static void ClearCache(Type[]? updatedTypes)
    {
        if (!Enabled) return;
        Clears++;
        TypesMatched = updatedTypes is not null
            && Array.Exists(updatedTypes, type => type == typeof(Callers))
            && Array.Exists(updatedTypes, type => type.Name == "NotificationSample");
    }

    public static void UpdateApplication(Type[]? updatedTypes)
    {
        if (!Enabled) return;
        if (Clears != Updates + 1) throw new InvalidOperationException("Cache clearing must precede application updates.");
        Updates++;
        throw new InvalidOperationException("notification-fixture-warning");
    }
}
