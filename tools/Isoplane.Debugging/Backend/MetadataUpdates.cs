using System;
using System.IO;
using System.Collections.Generic;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;

namespace Mono.Debugger.Soft;

public partial class ModuleMirror
{
    /// <summary>Applies compiler deltas to a paused guest and refreshes affected method and type metadata.</summary>
    /// <remarks>Active frames retain their invocation state; adopting edited instructions requires a supported statement jump.</remarks>
    public void ApplyChanges(byte[] metadata, byte[] il, byte[] symbols)
    {
        ArgumentNullException.ThrowIfNull(metadata);
        ArgumentNullException.ThrowIfNull(il);
        ArgumentNullException.ThrowIfNull(symbols);
        using var stream = new MemoryStream(metadata, writable: false);
        using var provider = MetadataReaderProvider.FromMetadataStream(stream);
        var reader = provider.GetMetadataReader();
        if (reader.GetGuid(reader.GetModuleDefinition().Mvid) != ModuleVersionId)
            throw new ArgumentException("The compiler delta does not match the loaded module baseline.", nameof(metadata));
        var domain = vm.RootDomain;
        ApplyChanges(domain.CreateByteArray(metadata), domain.CreateByteArray(il), domain.CreateByteArray(symbols));
        var changedTypes = new HashSet<TypeMirror>();
        foreach (var handle in reader.GetEditAndContinueMapEntries())
        {
            switch (handle.Kind)
            {
                case HandleKind.MethodDefinition:
                    var method = Assembly.GetMethod((uint)MetadataTokens.GetToken(handle));
                    method.RefreshAfterMetadataUpdate();
                    changedTypes.Add(method.DeclaringType);
                    break;
                case HandleKind.TypeDefinition:
                    changedTypes.Add(Assembly.GetType((uint)MetadataTokens.GetToken(handle)));
                    break;
            }
        }
        foreach (var entry in reader.GetEditAndContinueLogEntries())
            if (entry.Operation == EditAndContinueOperation.AddField)
                changedTypes.Add(Assembly.GetType((uint)MetadataTokens.GetToken(entry.Handle)));
        foreach (var type in changedTypes) type.RefreshAfterMetadataUpdate();
    }
}

public partial class MethodMirror
{
    internal void RefreshAfterMetadataUpdate()
    {
        SetUpdatedByEnC();
        ClearCachedLocalsDebugInfo();
        body = null;
        info = null;
        param_info = null;
        ret_param = null;
        cattrs = null;
#if ENABLE_CECIL
        meta = null;
#endif
    }
}

public partial class TypeMirror
{
    internal void RefreshAfterMetadataUpdate()
    {
        ClearCachedDebugInfo();
        methods = null;
        info = null;
        nested = null;
        cattrs = null;
        ifaces = null;
        iface_map = null;
#if ENABLE_CECIL
        meta = null;
#endif
    }
}
