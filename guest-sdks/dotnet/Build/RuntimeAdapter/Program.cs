using Mono.Cecil;
using Mono.Cecil.Cil;

// <corelib in> <corelib out> <sockets in> <sockets out> <dns in> <dns out> <sdk> <crypto in> <crypto out>
//   the adaptation for a runtime whose CoreLib, sockets, DNS and cryptography
//   have no WISP source integration; the source-built runtime needs none.
if (args.Length != 9)
    throw new ArgumentException("Expected pinned runtime input/output pairs.");

AdaptRuntime(args[0], args[1], args[2], args[3], args[4], args[5], args[6]);
AdaptCryptography(args[6], args[7], args[8]);

static void RequireSeparateOutput(string input, string output, string what)
{
    if (Path.GetFullPath(input) == Path.GetFullPath(output))
        throw new ArgumentException($"The {what} package is immutable; output must be a separate file.");
}

static void AdaptRuntime(string coreLibIn, string coreLibOut, string socketsIn, string socketsOut, string dnsIn, string dnsOut, string sdkPath)
{
    RequireSeparateOutput(coreLibIn, coreLibOut, "runtime");
    using var module = ModuleDefinition.ReadModule(coreLibIn);
    var dispatch = module.Types.Single(type => type.FullName == "System.Threading.ThreadPoolWorkQueue")
        .Methods.Single(method => method.Name == "Dispatch" && method.Parameters.Count == 0
            && method.IsStatic && method.ReturnType.FullName == "System.Boolean" && method.HasBody);
    var thread = module.Types.Single(type => type.FullName == "System.Threading.Thread");
    // The same entry the source-built runtime defines, so the SDK reaches one name in both.
    if (thread.Methods.Any(method => method.Name == "RunWasiEventLoopOnce"))
        throw new InvalidOperationException("The input CoreLib already contains the WISP scheduler adaptation.");

    var pump = new MethodDefinition("RunWasiEventLoopOnce", MethodAttributes.Assembly | MethodAttributes.Static,
        module.TypeSystem.Void);
    thread.Methods.Add(pump);
    var schedule = Polling.Adapt(module);
    NativeImports.Adapt(module, thread);
    var il = pump.Body.GetILProcessor();
    il.Append(Instruction.Create(OpCodes.Call, schedule));
    il.Append(Instruction.Create(OpCodes.Call, dispatch));
    il.Append(Instruction.Create(OpCodes.Pop));
    il.Append(Instruction.Create(OpCodes.Ret));
    module.Write(coreLibOut);

    RequireSeparateOutput(socketsIn, socketsOut, "sockets runtime");
    using var sockets = ModuleDefinition.ReadModule(socketsIn);
    SocketPolling.Adapt(sockets);
    sockets.Write(socketsOut);

    RequireSeparateOutput(dnsIn, dnsOut, "DNS runtime");
    using var dns = ModuleDefinition.ReadModule(dnsIn);
    // The DNS bridge lives in the SDK, which is an input here only to resolve that reference.
    using var sdk = ModuleDefinition.ReadModule(sdkPath);
    NameResolution.Adapt(dns, sdk);
    dns.Write(dnsOut);
}

static void AdaptCryptography(string sdkPath, string cryptoIn, string cryptoOut)
{
    RequireSeparateOutput(cryptoIn, cryptoOut, "cryptography runtime");
    using var sdk = ModuleDefinition.ReadModule(sdkPath);
    using var cryptography = CryptoRuntime.Read(cryptoIn);
    StaticMethods.Forward(cryptography, sdk, "System.Security.Cryptography.RandomNumberGenerator", "Wisp.Runtime.Randomness");
    StaticMethods.Forward(cryptography, sdk, "System.Security.Cryptography.SHA256", "Wisp.Runtime.Sha256");
    StaticMethods.Forward(cryptography, sdk, "System.Security.Cryptography.HMACSHA256", "Wisp.Runtime.HmacSha256");
    StaticMethods.Forward(cryptography, sdk, "System.Security.Cryptography.RandomNumberGeneratorImplementation", "Wisp.Abi.InstanceRandomness");
    ManagedHashProviders.Adapt(cryptography, sdk);
    ManagedPbkdf2.Adapt(cryptography);
    CertificateValues.Adapt(cryptography, sdk);
    cryptography.Write(cryptoOut);
}
