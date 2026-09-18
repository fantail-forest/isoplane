#if ISOPLANE_MONO
using System;
using System.IO;
using System.Reflection;
using System.Reflection.Metadata;
using System.Reflection.Metadata.Ecma335;
using System.Reflection.PortableExecutable;
using System.Threading.Tasks;

namespace Acceptance;

internal sealed partial class Reflection
{
    private static async Task<string> TrustedIdentityAfterPark(string component)
    {
        var control = new AssemblyName("Isoplane.Boundary.IdentityControl") { Version = new Version(1, 0, 0, 0) };
        if (Assembly.Load(Impostor(control)).GetName().Name != control.Name)
            throw new InvalidOperationException("impostor generator must produce a loadable ordinary assembly");
        Assembly original = component switch
        {
            "corelib" => typeof(object).Assembly,
            "bcl" => typeof(System.Net.Http.HttpClient).Assembly,
            "sdk" => typeof(Isoplane.Request).Assembly,
            _ => throw new ArgumentException("unknown trusted component"),
        };
        string before = PreserveDefaultIdentity(original);
        await Task.Delay(50).ConfigureAwait(false);
        return before + "/" + PreserveDefaultIdentity(original);
    }

    private static string PreserveDefaultIdentity(Assembly original)
    {
        AssemblyName identity = original.GetName();
        Assembly? candidate = null;
        try { candidate = Assembly.Load(Impostor(identity)); }
        catch (FileLoadException) { candidate = null; }
        if (candidate is not null && candidate.FullName != original.FullName)
            throw new InvalidOperationException("impostor fixture has the wrong assembly identity");
        if (!ReferenceEquals(Assembly.Load(identity), original))
            throw new InvalidOperationException("a byte-loaded assembly replaced the trusted default binding");
        if (ReferenceEquals(original, typeof(object).Assembly) && candidate is not null && !ReferenceEquals(candidate, original))
            throw new InvalidOperationException("an alternate core library was admitted");
        return "original";
    }

    private static byte[] Impostor(AssemblyName identity)
    {
        var metadata = new MetadataBuilder();
        metadata.AddModule(0, metadata.GetOrAddString(identity.Name + ".dll"), metadata.GetOrAddGuid(Guid.NewGuid()), default, default);
        byte[] key = identity.GetPublicKey() ?? Array.Empty<byte>();
        metadata.AddAssembly(metadata.GetOrAddString(identity.Name!), identity.Version!,
            metadata.GetOrAddString(identity.CultureName ?? ""), metadata.GetOrAddBlob(key),
            key.Length == 0 ? 0 : AssemblyFlags.PublicKey, AssemblyHashAlgorithm.None);
        metadata.AddTypeDefinition(TypeAttributes.NotPublic, default, metadata.GetOrAddString("<Module>"), default,
            MetadataTokens.FieldDefinitionHandle(1), MetadataTokens.MethodDefinitionHandle(1));
        var image = new ManagedPEBuilder(new PEHeaderBuilder(imageCharacteristics: Characteristics.ExecutableImage | Characteristics.Dll),
            new MetadataRootBuilder(metadata), new BlobBuilder(), flags: CorFlags.ILOnly);
        var bytes = new BlobBuilder();
        image.Serialize(bytes);
        return bytes.ToArray();
    }
}
#endif
