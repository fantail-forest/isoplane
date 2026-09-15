using Microsoft.Extensions.Configuration;

namespace Wisp.AspNetCore;

internal sealed class RuntimeConfigurationSource : IConfigurationSource
{
    public IConfigurationProvider Build(IConfigurationBuilder builder) => new Provider();

    private sealed class Provider : ConfigurationProvider
    {
        public override void Load()
        {
            LoadSettings();
            if (!Activation.IsActivated)
                Activation.Activated += LoadSettings;
        }

        private void LoadSettings()
        {
            Activation.Activated -= LoadSettings;
            Data = RuntimeConfiguration.ReadValues().ToDictionary(pair => pair.Key, pair => (string?)pair.Value,
                StringComparer.OrdinalIgnoreCase);
            OnReload();
        }
    }
}
