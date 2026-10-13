// Compiled into an application that references Microsoft.AspNetCore.DataProtection.EntityFrameworkCore,
// so that Isoplane.AspNetCore itself does not depend on Entity Framework Core.
#nullable enable
using System.Linq;
using System.Xml.Linq;
using Microsoft.AspNetCore.DataProtection;
using Microsoft.AspNetCore.DataProtection.EntityFrameworkCore;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.DependencyInjection;

namespace Isoplane.AspNetCore;

internal static class DataProtectionDbContextKeys
{
    /// <summary>
    /// Keeps the Data Protection keys in the <c>DataProtectionKeys</c> table of
    /// <typeparamref name="TContext"/>, as ASP.NET Core's <c>PersistKeysToDbContext</c> does, but
    /// reaching the database asynchronously, which the sandbox requires. The table has the same
    /// shape, so the application can share its keys with one hosted elsewhere.
    /// </summary>
    /// <remarks>
    /// The keys load when initialization finishes; see <see cref="DataProtectionKeys.UseKeysFrom"/>
    /// for when to load them earlier and how long a build keeps them.
    /// </remarks>
    public static IDataProtectionBuilder UseKeysFromDbContext<TContext>(this IDataProtectionBuilder builder)
        where TContext : DbContext, IDataProtectionKeyContext =>
        builder.UseKeysFrom(
            async (services, cancellation) =>
            {
                var keys = await services.GetRequiredService<TContext>().DataProtectionKeys.AsNoTracking().ToListAsync(cancellation);
                return keys.Where(key => !string.IsNullOrEmpty(key.Xml)).Select(key => XElement.Parse(key.Xml!)).ToList();
            },
            async (services, element, name, cancellation) =>
            {
                var context = services.GetRequiredService<TContext>();
                context.DataProtectionKeys.Add(new DataProtectionKey { FriendlyName = name, Xml = element.ToString(SaveOptions.DisableFormatting) });
                await context.SaveChangesAsync(cancellation);
            });
}
