using System.Security.Cryptography;
using System.Text;
using System.Xml.Linq;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.DataProtection;
using Microsoft.AspNetCore.DataProtection.AuthenticatedEncryption;
using Microsoft.AspNetCore.DataProtection.AuthenticatedEncryption.ConfigurationModel;
using Microsoft.AspNetCore.DataProtection.KeyManagement;
using Microsoft.AspNetCore.DataProtection.Repositories;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.DependencyInjection.Extensions;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;

namespace Isoplane.AspNetCore;

/// <summary>
/// Keeps ASP.NET Core Data Protection keys across template preparations. Without one of these,
/// a key created during initialization is captured in the template and shared by its instances.
/// Fresh initialization may create a different key; reusing a template retains its captured key.
/// Data protected with an earlier key needs that key to remain readable.
/// </summary>
/// <remarks>
/// The key stores ASP.NET Core ships for a database or another service, such as
/// <c>PersistKeysToDbContext</c>, do not work in the sandbox: Data Protection reads and writes
/// its keys synchronously, and an application cannot block on the network.
/// </remarks>
public static class DataProtectionKeys
{
    /// <summary>The configuration section <see cref="UseConfiguredKeys"/> reads when none is named.</summary>
    public const string DefaultSection = "DataProtection:Keys";

    /// <summary>
    /// Derives the keys from secrets in the application's settings, so every build given the same
    /// settings protects and unprotects with the same keys.
    /// </summary>
    /// <param name="builder">What <c>AddDataProtection</c> returned.</param>
    /// <param name="section">
    /// The section holding the secrets, as <c>DataProtection:Keys:0</c>, <c>DataProtection:Keys:1</c>
    /// and so on. Each is at least 32 random bytes in Base64. The first protects new data and every
    /// one unprotects, so to replace a key, put the new secret first and remove the old one once
    /// nothing it protected is still in use.
    /// </param>
    /// <returns><paramref name="builder"/>.</returns>
    /// <exception cref="InvalidOperationException">
    /// Thrown when the application starts, if the section holds no secret or one that is not
    /// Base64 of at least 32 bytes.
    /// </exception>
    /// <example>
    /// <code>
    /// builder.Services.AddDataProtection().UseConfiguredKeys();
    /// </code>
    /// Create a secret for local development with
    /// <c>dotnet user-secrets set "DataProtection:Keys:0" "$(openssl rand -base64 48)"</c>.
    /// </example>
    public static IDataProtectionBuilder UseConfiguredKeys(this IDataProtectionBuilder builder, string section = DefaultSection)
    {
        builder.Services.RemoveAll<IKeyManager>();
        builder.Services.AddSingleton<IKeyManager>(services => new ConfiguredKeyManager(
            services.GetRequiredService<IConfiguration>().GetSection(section), services.GetRequiredService<ILoggerFactory>()));
        builder.Services.AddHostedService<ConfiguredKeyCheck>();
        return builder;
    }

    /// <summary>
    /// Keeps the keys in a store the application reaches asynchronously, such as its database.
    /// The keys are loaded during initialization, and a new one is created and stored if none
    /// is usable. Instances use the keys captured in their template.
    /// </summary>
    /// <param name="builder">What <c>AddDataProtection</c> returned.</param>
    /// <param name="load">Returns every key element the store holds. It is given a service scope of its own.</param>
    /// <param name="store">Adds one key element, with the name Data Protection gives it, to the store. It shares <paramref name="load"/>'s scope.</param>
    /// <returns><paramref name="builder"/>.</returns>
    /// <remarks>
    /// Loading happens after every <c>Initialization.Initializing</c> callback has finished, so a
    /// callback that creates the store's tables runs first. A callback that itself protects or
    /// unprotects data, for instance by warming up a page with a form, awaits
    /// <see cref="LoadDataProtectionKeysAsync"/> before it does. Each prepared template keeps its
    /// captured keys, even after they expire. Preparing a new template may load newer keys;
    /// reusing a template reloads none.
    /// </remarks>
    public static IDataProtectionBuilder UseKeysFrom(
        this IDataProtectionBuilder builder,
        Func<IServiceProvider, CancellationToken, Task<IReadOnlyCollection<XElement>>> load,
        Func<IServiceProvider, XElement, string, CancellationToken, Task> store)
    {
        builder.Services.RemoveAll<LoadedKeyStore>();
        builder.Services.AddSingleton(services => new LoadedKeyStore(services, load, store));
        builder.Services.AddOptions<KeyManagementOptions>().Configure<LoadedKeyStore>((options, keys) =>
        {
            options.XmlRepository = keys;
            // Keys are created when the store is loaded, where a new one can be saved.
            options.AutoGenerateKeys = false;
        });
        // Data Protection reads its key ring as the host starts, before the store can have been loaded.
        foreach (var startup in builder.Services.Where(service => service.ImplementationType?.FullName == LoadedKeyStore.StartupService).ToList())
            builder.Services.Remove(startup);
        return builder;
    }

    /// <summary>
    /// Loads the keys of a store registered with <see cref="UseKeysFrom"/> now, instead of when
    /// initialization finishes. Await it in an <c>Initialization.Initializing</c> callback before
    /// anything there protects or unprotects data, and after whatever creates the store's tables.
    /// </summary>
    /// <param name="application">The built application.</param>
    /// <exception cref="InvalidOperationException">Thrown if the application registered no such store.</exception>
    public static Task LoadDataProtectionKeysAsync(this WebApplication application) =>
        (application.Services.GetService<LoadedKeyStore>()
            ?? throw new InvalidOperationException("No Data Protection key store is registered. Call UseKeysFrom on what AddDataProtection returns.")).LoadAsync();
}

/// <summary>Keys derived from configured secrets: the same secrets give the same keys in every build.</summary>
internal sealed class ConfiguredKeyManager : IKeyManager
{
    // A fixed instant in the past: a derived key is usable at once, and the earliest secret in the section activates last.
    private static readonly DateTimeOffset Epoch = new(2020, 1, 1, 0, 0, 0, TimeSpan.Zero);
    private readonly IReadOnlyCollection<IKey> keys;

    internal ConfiguredKeyManager(IConfigurationSection section, ILoggerFactory loggers)
    {
        var secrets = section.GetChildren().OrderBy(child => int.TryParse(child.Key, out var index) ? index : int.MaxValue).ToList();
        if (secrets.Count == 0)
            throw new InvalidOperationException($"No Data Protection key is configured. Set '{section.Path}:0' to at least 32 random bytes in Base64.");
        var factory = new AuthenticatedEncryptorFactory(loggers);
        keys = secrets.Select((secret, index) => (IKey)new ConfiguredKey(Decode(secret), Epoch.AddSeconds(secrets.Count - index), factory)).ToList();
    }

    public IReadOnlyCollection<IKey> GetAllKeys() => keys;

    public CancellationToken GetCacheExpirationToken() => CancellationToken.None;

    public IKey CreateNewKey(DateTimeOffset activationDate, DateTimeOffset expirationDate) => throw Fixed();

    public void RevokeKey(Guid keyId, string? reason = null) => throw Fixed();

    public void RevokeAllKeys(DateTimeOffset revocationDate, string? reason = null) => throw Fixed();

    private static NotSupportedException Fixed() =>
        new("Data Protection keys come from the application's settings; change the configured secrets to add or retire one.");

    private static byte[] Decode(IConfigurationSection secret)
    {
        var bytes = new byte[(secret.Value?.Length ?? 0) * 3 / 4 + 3];
        return Convert.TryFromBase64String(secret.Value ?? "", bytes, out var length) && length >= 32
            ? bytes[..length]
            : throw new InvalidOperationException($"The Data Protection key '{secret.Path}' must be at least 32 random bytes in Base64.");
    }

    private sealed class ConfiguredKey : IKey
    {
        private readonly AuthenticatedEncryptorFactory factory;

        internal ConfiguredKey(byte[] secret, DateTimeOffset activation, AuthenticatedEncryptorFactory factory)
        {
            this.factory = factory;
            KeyId = new Guid(HKDF.DeriveKey(HashAlgorithmName.SHA256, secret, 16, info: Encoding.UTF8.GetBytes("Isoplane.AspNetCore Data Protection key id")));
            var master = HKDF.DeriveKey(HashAlgorithmName.SHA256, secret, 64, info: Encoding.UTF8.GetBytes("Isoplane.AspNetCore Data Protection master key"));
            Descriptor = new AuthenticatedEncryptorDescriptor(new AuthenticatedEncryptorConfiguration(), new Secret(master));
            ActivationDate = CreationDate = activation;
        }

        public DateTimeOffset ActivationDate { get; }
        public DateTimeOffset CreationDate { get; }
        // Far enough ahead never to matter, without the arithmetic overflow DateTimeOffset.MaxValue invites.
        public DateTimeOffset ExpirationDate => Epoch.AddYears(1000);
        public bool IsRevoked => false;
        public Guid KeyId { get; }
        public IAuthenticatedEncryptorDescriptor Descriptor { get; }
        public IAuthenticatedEncryptor? CreateEncryptor() => factory.CreateEncryptorInstance(this);
    }
}

/// <summary>Fails the application's start, and with it preparation, when its configured keys are unusable.</summary>
internal sealed class ConfiguredKeyCheck(IKeyManager keys) : IHostedService
{
    public Task StartAsync(CancellationToken cancellationToken) => Task.FromResult(keys.GetAllKeys());

    public Task StopAsync(CancellationToken cancellationToken) => Task.CompletedTask;
}

/// <summary>
/// The key elements of an asynchronous store, held in memory for Data Protection's synchronous
/// reads. Elements Data Protection adds while the store is being loaded are saved to it then.
/// </summary>
internal sealed class LoadedKeyStore(
    IServiceProvider services,
    Func<IServiceProvider, CancellationToken, Task<IReadOnlyCollection<XElement>>> load,
    Func<IServiceProvider, XElement, string, CancellationToken, Task> store) : IXmlRepository
{
    internal const string StartupService = "Microsoft.AspNetCore.DataProtection.Internal.DataProtectionHostedService";

    // Data Protection's own margin: a key this close to expiring is replaced before it does.
    private static readonly TimeSpan Renewal = TimeSpan.FromDays(2);
    private readonly List<(XElement Element, string Name)> added = [];
    private List<XElement>? elements;
    private Task? loading;
    private bool loaded;

    public IReadOnlyCollection<XElement> GetAllElements() =>
        (elements ?? throw new InvalidOperationException(
            "The Data Protection keys are not loaded yet. They load when initialization finishes; to protect or unprotect data in an Initialization.Initializing callback, await app.LoadDataProtectionKeysAsync() first."))
        .Select(element => new XElement(element)).ToList();

    public void StoreElement(XElement element, string friendlyName)
    {
        if (elements is null || loaded)
            throw new InvalidOperationException("Data Protection keys are created only while the key store loads, during initialization.");
        elements.Add(new XElement(element));
        added.Add((new XElement(element), friendlyName));
    }

    internal Task LoadAsync() => loading ??= LoadOnceAsync();

    private async Task LoadOnceAsync()
    {
        await using var scope = services.CreateAsyncScope();
        elements = [.. await load(scope.ServiceProvider, CancellationToken.None)];
        var manager = services.GetRequiredService<IKeyManager>();
        var now = DateTimeOffset.UtcNow;
        if (!manager.GetAllKeys().Any(key => !key.IsRevoked && key.ActivationDate <= now && key.ExpirationDate > now + Renewal))
            manager.CreateNewKey(now, now + services.GetRequiredService<IOptions<KeyManagementOptions>>().Value.NewKeyLifetime);
        foreach (var (element, name) in added)
            await store(scope.ServiceProvider, element, name, CancellationToken.None);
        loaded = true;
    }
}
