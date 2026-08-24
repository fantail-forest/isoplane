using System;
using System.Runtime.InteropServices;

namespace Wisp.Abi;

/// <summary>
/// The host import surface, module <c>"wisp"</c>.
///
/// These signatures are the C# side of <c>wisp-abi::imports::IMPORTS</c>
/// (spec.md §5.3, invariant 4). Every name and every signature here must
/// appear in that table verbatim; the deploy-time validator's rule V1 rejects
/// anything else, so a typo is a deploy failure rather than a runtime bug.
///
/// <c>[WasmImportLinkage]</c> is what makes the emitted wasm say
/// <c>(import "wisp" "kv_get" ...)</c> rather than routing through a
/// PInvoke stub. Without it the import lands in whatever module wasm-ld
/// defaults to and V1 rejects the module.
/// </summary>
internal static unsafe partial class H
{
    // ---- core ------------------------------------------------------------

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "abort")]
    internal static extern void Abort(byte* msg, int len);

    // ---- observability ---------------------------------------------------
    // §10: log CPU counts against the dispatch budget. Cheap, but not free.

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "log_emit")]
    internal static extern void LogEmit(int level, byte* ptr, int len);

    // ---- clock -----------------------------------------------------------
    // Coarse and frozen intra-slice (§8.5). Not init-visible: a cctor that
    // reads the clock is impure and is deferred to first touch (§4.4).

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "clock_now_coarse")]
    internal static extern long ClockNowCoarse();

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "clock_deadline_in")]
    internal static extern long ClockDeadlineIn(long ms);

    // ---- entropy ---------------------------------------------------------

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "entropy_fill")]
    internal static extern void EntropyFill(byte* ptr, int len);

    // ---- outbound http ---------------------------------------------------

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "http_fetch")]
    internal static extern long HttpFetch(byte* ptr, int len);

    // ---- inbound response ------------------------------------------------

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "resp_new")]
    internal static extern int RespNew(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("wisp", EntryPoint = "resp_finish")]
    internal static extern void RespFinish(int handle);
}
