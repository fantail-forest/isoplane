using Xunit;

public sealed class CryptoRuntimeTests
{
    [Fact]
    public void An_unpinned_crypto_input_is_rejected_before_assembly_loading()
    {
        string directory = Directory.CreateTempSubdirectory("wisp-crypto-input-").FullName;
        try
        {
            string path = Path.Combine(directory, "System.Security.Cryptography.dll");
            File.WriteAllBytes(path, new byte[] { 1, 2, 3 });
            var error = Assert.Throws<InvalidOperationException>(() => CryptoRuntime.Read(path));
            Assert.True(error.Message.Contains("restore the SDK's exact package input", StringComparison.Ordinal),
                "a rejected crypto input must tell the developer how to restore the supported assembly");
        }
        finally { Directory.Delete(directory, recursive: true); }
    }
}
