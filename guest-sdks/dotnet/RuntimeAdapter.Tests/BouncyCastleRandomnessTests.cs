using Mono.Cecil;
using Mono.Cecil.Cil;
using Org.BouncyCastle.Security;
using Xunit;

public sealed class BouncyCastleRandomnessTests
{
    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public void Changed_static_generator_initialization_is_rejected(bool changeSeed)
    {
        using var module = BouncyCastleRandomness.Read(typeof(SecureRandom).Assembly.Location);
        var random = module.Types.Single(type => type.FullName == typeof(SecureRandom).FullName);
        var initializer = random.Methods.Single(method => method.Name == ".cctor");
        var generator = initializer.Body.Instructions.Single(instruction => instruction.Operand is MethodReference method
            && method.DeclaringType.FullName == "Org.BouncyCastle.Crypto.Prng.VmpcRandomGenerator");
        if (changeSeed) generator.Next.Operand = (sbyte)17;
        else initializer.Body.GetILProcessor().Remove(generator);
        var error = Assert.Throws<InvalidOperationException>(() => BouncyCastleRandomness.Adapt(module));
        Assert.True(error.Message.StartsWith("Expected the pinned BouncyCastle static RNG", StringComparison.Ordinal),
            "unknown static generator initialization must be refused, not partially rewritten");
    }

    [Fact]
    public void An_unpinned_BouncyCastle_input_is_rejected_before_loading()
    {
        string directory = Directory.CreateTempSubdirectory("wisp-bc-input-").FullName;
        try
        {
            string path = Path.Combine(directory, "BouncyCastle.Cryptography.dll");
            byte[] original = File.ReadAllBytes(typeof(SecureRandom).Assembly.Location);
            original[^1] ^= 1;
            File.WriteAllBytes(path, original);
            var error = Assert.Throws<InvalidOperationException>(() => BouncyCastleRandomness.Read(path));
            Assert.True(error.Message.Contains("restore the SDK's exact package input", StringComparison.Ordinal),
                "an unsupported BouncyCastle input must name how to restore the pinned dependency");
        }
        finally { Directory.Delete(directory, recursive: true); }
    }
}
