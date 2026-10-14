using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.DataProtection;
using Microsoft.AspNetCore.DataProtection.EntityFrameworkCore;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Xunit;

namespace Isoplane.AspNetCore.DataProtection.EntityFrameworkCore.Tests;

public sealed class DataProtectionDbContextKeysTests
{
    private sealed class KeyContext(DbContextOptions<KeyContext> options) : DbContext(options), IDataProtectionKeyContext
    {
        public DbSet<DataProtectionKey> DataProtectionKeys => Set<DataProtectionKey>();
    }

    /// <summary>One build of an application whose keys are kept in <paramref name="database"/>, with them loaded.</summary>
    private static async Task<WebApplication> BuildAsync(string database)
    {
        var builder = IsoplaneHosting.CreateBuilder([]);
        builder.Logging.ClearProviders();
        builder.Services.AddDbContext<KeyContext>(options => options.UseInMemoryDatabase(database));
        builder.Services.AddDataProtection().UseKeysFromDbContext<KeyContext>();
        var application = builder.Build();
        await application.LoadDataProtectionKeysAsync();
        return application;
    }

    private static IDataProtector Protector(WebApplication application) =>
        application.Services.GetRequiredService<IDataProtectionProvider>().CreateProtector("test");

    [Fact]
    public async Task builds_sharing_a_database_share_the_key_its_first_build_stored()
    {
        var database = Guid.NewGuid().ToString();
        await using var earlier = await BuildAsync(database);
        await using var later = await BuildAsync(database);
        Assert.Equal("cookie", Protector(later).Unprotect(Protector(earlier).Protect("cookie")));
        await using var scope = later.Services.CreateAsyncScope();
        Assert.Equal(1, await scope.ServiceProvider.GetRequiredService<KeyContext>().DataProtectionKeys.CountAsync());
    }
}
