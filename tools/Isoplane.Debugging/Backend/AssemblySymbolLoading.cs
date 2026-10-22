using System;
using System.Threading.Tasks;
using Mono.Debugger.Soft;

namespace Mono.Debugging.Soft;

public partial class SoftDebuggerSession
{
    private async Task LoadAssemblySymbolsAsync(AssemblyMirror assembly)
    {
        try
        {
            await DebugSymbolsManager.TryLoadSymbolFromSymbolServerIfNeeded(assembly);
        }
        catch (Exception error)
        {
            OnDebuggerOutput(true, $"Loading assembly symbols failed: {error}\n");
        }
    }
}
