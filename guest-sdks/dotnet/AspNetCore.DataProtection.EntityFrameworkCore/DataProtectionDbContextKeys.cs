using System.Xml.Linq;
using Microsoft.AspNetCore.DataProtection;
using Microsoft.AspNetCore.DataProtection.EntityFrameworkCore;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.DependencyInjection;

namespace Isoplane.AspNetCore.DataProtection.EntityFrameworkCore;

/// <summary>
/// Keeps ASP.NET Core Data Protection keys in an Entity Framework Core database, loading the shared
/// key ring during initialization and capturing it in the prepared template.
/// </summary>
public static class DataProtectionDbContextKeys
{
    /// <summary>
    /// Keeps the Data Protection keys in the <c>DataProtectionKeys</c> table of
    /// <typeparamref name="TContext"/>, as ASP.NET Core's <c>PersistKeysToDbContext</c> does, but
    /// reaching the database asynchronously, which the sandbox requires. The table has the same
    /// shape, so the application can share its keys with one hosted elsewhere.
    /// </summary>
    /// <typeparam name="TContext">The application's context, registered with <c>AddDbContext</c>.</typeparam>
    /// <param name="builder">What <c>AddDataProtection</c> returned.</param>
    /// <returns><paramref name="builder"/>.</returns>
    /// <remarks>
    /// This is <c>UseKeysFrom</c> from Isoplane.AspNetCore over the table, and behaves as it does.
    /// The keys load when initialization finishes, so the table must exist by then. An
    /// <c>Initialization.Initializing</c> callback that protects or unprotects data itself awaits
    /// <c>app.LoadDataProtectionKeysAsync()</c> first. A prepared template retains the key ring it
    /// loaded; reusing it reloads none. New initialization can load newer keys.
    /// <see href="https://isoplane.dev/docs/dotnet-runtime-differences#keep-data-protection-keys-across-builds">Keep Data Protection keys across builds</see>
    /// describes this in full.
    /// </remarks>
    /// <example>
    /// <code>
    /// builder.Services.AddDataProtection().UseKeysFromDbContext&lt;AppDbContext&gt;();
    /// </code>
    /// </example>
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
