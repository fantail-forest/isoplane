using System.Security.Cryptography;
using System.Xml.Linq;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.DataProtection;
using Microsoft.Extensions.Configuration.Memory;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class DataProtectionKeysTests
{
    private const string First = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
    private const string Second = "ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA=";

    /// <summary>One build of an application: a started host whose services were set up by <paramref name="configure"/>.</summary>
    private static async Task<WebApplication> BuildAsync(Action<WebApplicationBuilder> configure, params string[] secrets)
    {
        var settings = secrets.Select((secret, index) => KeyValuePair.Create($"DataProtection:Keys:{index}", (string?)secret));
        var builder = IsoplaneHosting.CreateBuilder("App", new MemoryConfigurationSource { InitialData = settings });
        builder.Logging.ClearProviders();
        builder.Services.AddSingleton<Func<ResponseTransport>>(() => new RecordingResponseTransport());
        configure(builder);
        var application = builder.Build();
        await application.StartAsync();
        return application;
    }

    private static IDataProtector Protector(WebApplication application) =>
        application.Services.GetRequiredService<IDataProtectionProvider>().CreateProtector("test");

    private static Task<WebApplication> ConfiguredAsync(params string[] secrets) =>
        BuildAsync(builder => builder.Services.AddDataProtection().UseConfiguredKeys(), secrets);

    [Fact]
    public async Task builds_given_the_same_configured_secret_read_what_each_other_protected()
    {
        await using var earlier = await ConfiguredAsync(First);
        await using var later = await ConfiguredAsync(First);
        Assert.Equal("cookie", Protector(later).Unprotect(Protector(earlier).Protect("cookie")));
    }

    [Fact]
    public async Task the_first_configured_secret_protects_and_a_retained_one_still_unprotects()
    {
        await using var before = await ConfiguredAsync(First);
        await using var rotating = await ConfiguredAsync(Second, First);
        await using var after = await ConfiguredAsync(Second);
        var old = Protector(before).Protect("old cookie");
        var current = Protector(rotating).Protect("new cookie");
        Assert.Equal("old cookie", Protector(rotating).Unprotect(old));
        Assert.Equal("new cookie", Protector(after).Unprotect(current));
        Assert.Throws<CryptographicException>(() => Protector(after).Unprotect(old));
    }

    [Theory]
    [InlineData]
    [InlineData("dG9vIHNob3J0")]
    [InlineData("not base64 at all, however long it happens to be")]
    public async Task an_application_without_a_usable_configured_secret_does_not_start(params string[] secrets)
    {
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => ConfiguredAsync(secrets));
        Assert.Contains("DataProtection:Keys:0", error.Message, StringComparison.Ordinal);
    }

    private static Task<WebApplication> StoredAsync(List<XElement> stored) => BuildAsync(builder =>
        builder.Services.AddDataProtection().UseKeysFrom(
            (_, _) => Task.FromResult<IReadOnlyCollection<XElement>>(stored.ToList()),
            (_, element, _, _) => { stored.Add(element); return Task.CompletedTask; }));

    [Fact]
    public async Task a_store_gains_one_key_from_its_first_build_and_later_builds_use_it()
    {
        var stored = new List<XElement>();
        await using var earlier = await StoredAsync(stored);
        await earlier.LoadDataProtectionKeysAsync();
        Assert.Single(stored);
        await using var later = await StoredAsync(stored);
        await later.LoadDataProtectionKeysAsync();
        Assert.Single(stored);
        Assert.Equal("cookie", Protector(later).Unprotect(Protector(earlier).Protect("cookie")));
    }

    [Fact]
    public async Task finishing_initialization_loads_a_store_the_application_did_not_load_itself()
    {
        var stored = new List<XElement>();
        await using var application = await StoredAsync(stored);
        await (await application.StartedAsync()).LoadDataProtectionKeysAsync();
        Assert.Single(stored);
        Assert.Equal("cookie", Protector(application).Unprotect(Protector(application).Protect("cookie")));
    }

    [Fact]
    public async Task data_protection_does_not_read_a_store_as_the_host_starts()
    {
        Assert.NotNull(typeof(IDataProtector).Assembly.GetType(LoadedKeyStore.StartupService) ?? typeof(DataProtectionOptions).Assembly.GetType(LoadedKeyStore.StartupService));
        await using var application = await StoredAsync([]);
        Assert.DoesNotContain(application.Services.GetServices<Microsoft.Extensions.Hosting.IHostedService>(),
            service => service.GetType().FullName == LoadedKeyStore.StartupService);
    }

    [Fact]
    public async Task protecting_before_a_store_has_loaded_says_how_to_load_it()
    {
        await using var application = await StoredAsync([]);
        var error = Record.Exception(() => Protector(application).Protect("cookie"));
        var messages = new List<string>();
        for (var cause = error; cause is not null; cause = cause.InnerException) messages.Add(cause.Message);
        Assert.Contains(messages, message => message.Contains("LoadDataProtectionKeysAsync", StringComparison.Ordinal));
    }
}
