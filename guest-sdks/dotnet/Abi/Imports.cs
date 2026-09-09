using System;
using System.Runtime.InteropServices;

namespace Wisp.Abi;

/// <summary>
/// The host imports the SDK reaches, module <c>"wisp"</c>. Every entry is
/// held to <c>wisp-abi::imports::IMPORTS</c> by
/// <c>crates/wisp-abi/tests/dotnet_sdk_drift.rs</c>, and each has a wrapper
/// in <c>Sdk/</c>; an import with no wrapper is a capability the SDK does not
/// offer. <c>[WasmImportLinkage]</c> is what makes the emitted wasm import
/// from module <c>wisp</c> rather than through a PInvoke stub.
/// </summary>
internal static unsafe partial class H
{
    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "entropy_fill")]
    internal static extern void EntropyFill(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "init_config_get")]
    internal static extern int InitConfigGet(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "abort")]
    internal static extern void Abort(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "log_emit")]
    internal static extern void LogEmit(int level, byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "resp_new")]
    internal static extern int RespNew(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "resp_write")]
    internal static extern long RespWrite(int handle, byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "resp_finish")]
    internal static extern void RespFinish(int handle);
}
