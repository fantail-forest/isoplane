using System.Security.Cryptography;
using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class BouncyCastleRandomness
{
    private const string InputSha256 = "EF92FC661E8D7BA8EC4D39A7CDFCDBA41F14CB327F0D24FE79D0B5BB428899B5";

    internal static ModuleDefinition Read(string path)
    {
        byte[] input = File.ReadAllBytes(path);
        if (Convert.ToHexString(SHA256.HashData(input)) != InputSha256)
            throw new InvalidOperationException("Expected the pinned BouncyCastle.Cryptography 2.7.0 net6.0 assembly; restore the SDK's exact package input.");
        return ModuleDefinition.ReadModule(new MemoryStream(input, writable: false));
    }

    internal static void Adapt(ModuleDefinition module)
    {
        var random = module.Types.Single(type => type.FullName == "Org.BouncyCastle.Security.SecureRandom");
        var generator = module.Types.Single(type => type.FullName == "Org.BouncyCastle.Crypto.Prng.CryptoApiRandomGenerator");
        var hostGenerator = RuntimeMethods.Require(generator, ".ctor", false, "System.Void");
        var supplied = RuntimeMethods.Require(random, ".ctor", false, "System.Void", "Org.BouncyCastle.Crypto.Prng.IRandomGenerator");
        var defaultRandom = RuntimeMethods.Require(random, ".ctor", false, "System.Void");
        var initializer = RuntimeMethods.Require(random, ".cctor", true, "System.Void");
        var candidates = initializer.Body.Instructions.Where(instruction => instruction.OpCode == OpCodes.Newobj
            && instruction.Operand is MethodReference method
            && method.FullName == "System.Void Org.BouncyCastle.Crypto.Prng.VmpcRandomGenerator::.ctor()").ToArray();
        if (candidates.Length != 1)
            throw new InvalidOperationException("Expected the pinned BouncyCastle static RNG generator initialization.");
        var construct = candidates[0];
        var seedLength = construct.Next;
        var seeded = seedLength?.Next;
        if (seedLength?.OpCode != OpCodes.Ldc_I4_S || seedLength.Operand is not sbyte length || length != 16
            || seeded?.OpCode != OpCodes.Newobj || seeded.Operand is not MethodReference constructor
            || constructor.FullName != "System.Void Org.BouncyCastle.Security.SecureRandom::.ctor(Org.BouncyCastle.Crypto.Prng.IRandomGenerator,System.Int32)")
            throw new InvalidOperationException("Expected the pinned BouncyCastle static RNG seed initialization.");

        // Both defaults must draw through the OS-backed provider after cloning a warm heap.
        construct.Operand = hostGenerator;
        var initializeIl = initializer.Body.GetILProcessor();
        initializeIl.Remove(seedLength);
        seeded.Operand = supplied;

        defaultRandom.Body = new MethodBody(defaultRandom);
        var il = defaultRandom.Body.GetILProcessor();
        il.Emit(OpCodes.Ldarg_0);
        il.Emit(OpCodes.Newobj, hostGenerator);
        il.Emit(OpCodes.Call, supplied);
        il.Emit(OpCodes.Ret);
    }
}
