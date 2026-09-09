using Mono.Cecil;
using Mono.Cecil.Cil;

if (args.Length != 2 && args.Length != 4 && args.Length != 9 && args.Length != 11)
    throw new ArgumentException("Expected pinned runtime input/output pairs.");
if (Path.GetFullPath(args[0]) == Path.GetFullPath(args[1]))
    throw new ArgumentException("The runtime package is immutable; output must be a separate file.");

using var module = ModuleDefinition.ReadModule(args[0]);
var dispatch = module.Types.Single(type => type.FullName == "System.Threading.ThreadPoolWorkQueue")
    .Methods.Single(method => method.Name == "Dispatch" && method.Parameters.Count == 0
        && method.IsStatic && method.ReturnType.FullName == "System.Boolean" && method.HasBody);
var thread = module.Types.Single(type => type.FullName == "System.Threading.Thread");
if (thread.Methods.Any(method => method.Name == "WispRunReady"))
    throw new InvalidOperationException("The input CoreLib already contains the WISP scheduler adaptation.");

var pump = new MethodDefinition("WispRunReady", MethodAttributes.Assembly | MethodAttributes.Static,
    module.TypeSystem.Void);
thread.Methods.Add(pump);
var schedule = Polling.Adapt(module);
NativeImports.Adapt(module, thread);
var il = pump.Body.GetILProcessor();
il.Append(Instruction.Create(OpCodes.Call, schedule));
il.Append(Instruction.Create(OpCodes.Call, dispatch));
il.Append(Instruction.Create(OpCodes.Pop));
il.Append(Instruction.Create(OpCodes.Ret));
var register = module.Types.Single(type => type.FullName == "System.Threading.WasiEventLoop")
    .Methods.Single(method => method.Name == "RegisterWasiPollableHandle");
var wait = new MethodDefinition("WispWaitPollable", MethodAttributes.Assembly | MethodAttributes.Static, register.ReturnType);
thread.Methods.Add(wait);
var waitIl = wait.Body.GetILProcessor();
foreach (var parameter in register.Parameters)
{
    var copy = new ParameterDefinition(parameter.ParameterType);
    wait.Parameters.Add(copy);
    waitIl.Emit(OpCodes.Ldarg, copy);
}
waitIl.Emit(OpCodes.Call, register);
waitIl.Emit(OpCodes.Ret);
module.Write(args[1]);

if (args.Length >= 4)
{
    if (Path.GetFullPath(args[2]) == Path.GetFullPath(args[3]))
        throw new ArgumentException("The sockets runtime package is immutable; output must be separate.");
    using var sockets = ModuleDefinition.ReadModule(args[2]);
    SocketPolling.Adapt(sockets);
    sockets.Write(args[3]);
}

if (args.Length >= 9)
{
    if (Path.GetFullPath(args[4]) == Path.GetFullPath(args[5]))
        throw new ArgumentException("The DNS runtime package is immutable; output must be separate.");
    using var dns = ModuleDefinition.ReadModule(args[4]);
    using var sdk = ModuleDefinition.ReadModule(args[6]);
    NameResolution.Adapt(dns, sdk);
    dns.Write(args[5]);
    if (Path.GetFullPath(args[7]) == Path.GetFullPath(args[8]))
        throw new ArgumentException("The cryptography runtime package is immutable; output must be separate.");
    using var cryptography = CryptoRuntime.Read(args[7]);
    StaticMethods.Forward(cryptography, sdk, "System.Security.Cryptography.RandomNumberGenerator", "Wisp.Runtime.Randomness");
    StaticMethods.Forward(cryptography, sdk, "System.Security.Cryptography.SHA256", "Wisp.Runtime.Sha256");
    StaticMethods.Forward(cryptography, sdk, "System.Security.Cryptography.HMACSHA256", "Wisp.Runtime.HmacSha256");
    StaticMethods.Forward(cryptography, sdk, "System.Security.Cryptography.RandomNumberGeneratorImplementation", "Wisp.Abi.InstanceRandomness");
    ManagedHashProviders.Adapt(cryptography, sdk);
    ManagedPbkdf2.Adapt(cryptography);
    CertificateValues.Adapt(cryptography, sdk);
    cryptography.Write(args[8]);
}

if (args.Length == 11)
{
    if (Path.GetFullPath(args[9]) == Path.GetFullPath(args[10]))
        throw new ArgumentException("The BouncyCastle package is immutable; output must be separate.");
    using var bouncyCastle = BouncyCastleRandomness.Read(args[9]);

    BouncyCastleRandomness.Adapt(bouncyCastle);
    bouncyCastle.Write(args[10]);
}
