using Microsoft.Extensions.Configuration;

namespace Isoplane.AspNetCore;

internal sealed class RuntimeConfigurationSource : IConfigurationSource
{
    public IConfigurationProvider Build(IConfigurationBuilder builder) => new Provider();

    private sealed class Provider : ConfigurationProvider
    {
        public override void Load()
        {
            LoadSettings();
            if (!Instance.HasStarted)
                Instance.Starting += ReloadAsync;
        }

        private Task ReloadAsync()
        {
            LoadSettings();
            return Task.CompletedTask;
        }

        private void LoadSettings()
        {
            Instance.Starting -= ReloadAsync;
            Data = RuntimeConfiguration.ReadValues().ToDictionary(pair => pair.Key, pair => (string?)pair.Value,
                StringComparer.OrdinalIgnoreCase);
            OnReload();
        }
    }
}
