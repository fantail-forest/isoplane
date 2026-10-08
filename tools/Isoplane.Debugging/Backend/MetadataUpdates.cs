using System;
using System.IO;
using System.Linq;
using System.Collections.Generic;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;

namespace Mono.Debugger.Soft;

public partial class ModuleMirror
{
    private int appliedGeneration;
    private Guid appliedGenerationId;
    internal bool HasAppliedChanges => appliedGeneration != 0;

    /// <summary>Applies compiler deltas to a paused guest and refreshes affected method and type metadata.</summary>
    /// <remarks>Active frames retain their invocation state; adopting edited instructions requires a supported statement jump.</remarks>
    public void ApplyChanges(byte[] metadata, byte[] il, byte[] symbols, IEnumerable<SourceUpdate> sourceUpdates = null)
    {
        ArgumentNullException.ThrowIfNull(metadata);
        ArgumentNullException.ThrowIfNull(il);
        ArgumentNullException.ThrowIfNull(symbols);
        using var stream = new MemoryStream(metadata, writable: false);
        using var provider = MetadataReaderProvider.FromMetadataStream(stream);
        var reader = provider.GetMetadataReader();
        var module = reader.GetModuleDefinition();
        if (reader.GetGuid(module.Mvid) != ModuleVersionId)
            throw new ArgumentException("The compiler delta does not match the loaded module baseline.", nameof(metadata));
        if (module.Generation != appliedGeneration + 1
            || appliedGeneration != 0 && reader.GetGuid(module.BaseGenerationId) != appliedGenerationId)
            throw new ArgumentException("The compiler delta does not follow the last applied update.", nameof(metadata));
        var updatedTokens = reader.GetEditAndContinueMapEntries()
            .Where(handle => handle.Kind == HandleKind.MethodDefinition)
            .Select(MetadataTokens.GetToken).ToHashSet();
        foreach (var thread in vm.GetThreads())
            foreach (var frame in thread.GetFrames())
                if (frame.Method.DeclaringType.Assembly.ManifestModule == this && updatedTokens.Contains(frame.Method.MetadataToken))
                    frame.Method.PreserveVersion(frame.MethodVersion);
        var domain = vm.RootDomain;
        ApplyChanges(domain.CreateByteArray(metadata), domain.CreateByteArray(il), domain.CreateByteArray(symbols));
        appliedGeneration = module.Generation;
        appliedGenerationId = reader.GetGuid(module.GenerationId);
        var changedTypes = new HashSet<TypeMirror>();
        var changedMethods = new HashSet<MethodMirror>();
        foreach (var handle in reader.GetEditAndContinueMapEntries())
        {
            switch (handle.Kind)
            {
                case HandleKind.MethodDefinition:
                    var method = Assembly.GetMethod((uint)MetadataTokens.GetToken(handle));
                    method.RefreshAfterMetadataUpdate(module.Generation + 1);
                    changedMethods.Add(method);
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
        foreach (var update in sourceUpdates ?? Array.Empty<SourceUpdate>())
            foreach (var type in vm.GetTypesForSourceFile(update.FileName, false))
                if (type.Assembly.ManifestModule == this)
                    foreach (var method in type.GetMethods())
                        if (!changedMethods.Contains(method)) method.ApplySequencePointChanges(update);
    }
}

public partial class MethodMirror
{
    private bool? hasVariableArguments;

    /// <summary>Whether this method accepts a variable number of arguments.</summary>
    public bool HasVariableArguments => hasVariableArguments ??= new SignatureHeader(checked((byte)vm.conn.Method_GetParamInfo(id).call_conv))
        .CallingConvention == SignatureCallingConvention.VarArgs;

    internal void ApplySequencePointChanges(SourceUpdate update)
    {
        // Compiler line maps describe ranges of unchanged instructions; changed methods get fresh PDBs instead.
        var lines = update.LineUpdates.OrderBy(line => line.Item1).ToArray();
        int Map(int line)
        {
            if (line <= 0 || line == 0xfeefee) return line;
            var boundary = lines.LastOrDefault(entry => entry.Item1 < line);
            return boundary is null ? line : line + boundary.Item2 - boundary.Item1;
        }
        _ = Locations;
        for (int index = 0; index < debug_info.line_numbers.Length; index++)
        {
            if (!string.Equals(debug_info.source_files[index].source_file, update.FileName, StringComparison.Ordinal)) continue;
            debug_info.line_numbers[index] = Map(debug_info.line_numbers[index]);
            debug_info.end_line_numbers[index] = Map(debug_info.end_line_numbers[index]);
        }
        locations = null;
    }

    internal void RefreshAfterMetadataUpdate(int version)
    {
        CurrentVersion = version;
        SetUpdatedByEnC();
        ClearCachedLocalsDebugInfo();
        body = null;
        info = null;
        param_info = null;
        hasVariableArguments = null;
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
