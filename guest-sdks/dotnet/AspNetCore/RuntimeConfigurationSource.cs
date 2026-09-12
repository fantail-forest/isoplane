using Microsoft.Extensions.Configuration;

namespace Wisp.AspNetCore;

internal sealed class RuntimeConfigurationSource : IConfigurationSource
{
    public IConfigurationProvider Build(IConfigurationBuilder builder) => new Provider();

    private sealed class Provider : ConfigurationProvider
    {
        public override void Load()
        {
            if (Activation.IsActivated)
                LoadActivated();
            else
                Activation.Activated += LoadActivated;
        }

        private void LoadActivated()
        {
            Activation.Activated -= LoadActivated;
            Data = RuntimeConfiguration.ReadValues().ToDictionary(pair => pair.Key, pair => (string?)pair.Value,
                StringComparer.OrdinalIgnoreCase);
            OnReload();
        }
    }
}
