using System;
using System.Runtime.InteropServices;

namespace Isoplane.Abi;

/// <summary>
/// The host imports the SDK reaches, module <c>"isoplane"</c>. Every entry is
/// held to <c>isoplane-abi::imports::IMPORTS</c> by
/// <c>crates/isoplane-abi/tests/dotnet_sdk_drift.rs</c>, and each has a wrapper
/// in <c>Sdk/</c>; an import with no wrapper is a capability the SDK does not
/// offer. <c>[WasmImportLinkage]</c> is what makes the emitted wasm import
/// from module <c>isoplane</c> rather than through a PInvoke stub.
/// </summary>
internal static unsafe partial class H
{
    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "entropy_fill")]
    internal static extern void EntropyFill(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "runtime_config_get")]
    internal static extern int RuntimeConfigGet(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "abort")]
    internal static extern void Abort(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "log_emit")]
    internal static extern void LogEmit(int level, byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "req_body_read")]
    internal static extern long ReqBodyRead(int maxBytes);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "req_body_can_have_body")]
    internal static extern int ReqBodyCanHaveBody();

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "resp_new")]
    internal static extern int RespNew(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "resp_write")]
    internal static extern long RespWrite(int handle, byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "resp_finish")]
    internal static extern void RespFinish(int handle);
}
