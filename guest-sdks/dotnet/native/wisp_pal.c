/*
 * The WISP platform abstraction layer for a NativeAOT-LLVM guest.
 *
 * These canonical-ABI symbols route the provided WASI operations through host
 * imports. Unsupported entries abort with their name; the compiled guest's
 * imports and signatures still require validation against the host interface.
 * Clock reads use the host's coarse clock, fixed per serving slice.
 * Entropy comes through the host import. Values retained in captured guest
 * memory remain captured; a later draw does not rewrite earlier values.
 */

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* ---- the WISP imports this PAL is built from --------------------------- */

__attribute__((import_module("wisp"), import_name("abort")))
extern void wisp_abort(const uint8_t *msg, int32_t len);

__attribute__((import_module("wisp"), import_name("clock_now_coarse")))
extern int64_t wisp_clock_now_coarse(void);

__attribute__((import_module("wisp"), import_name("entropy_fill")))
extern void wisp_entropy_fill(uint8_t *buf, int32_t len);

/* Name an unsupported PAL entry before trapping so the failure is diagnosable. */
#define WISP_PAL_DEAD(name)                                                    \
    do {                                                                       \
        static const char m[] = "wisp: guest reached unsupported PAL entry "    \
                                name;                                          \
        wisp_abort((const uint8_t *)m, (int32_t)(sizeof(m) - 1));               \
        __builtin_unreachable();                                               \
    } while (0)

/* ---- live: environment -------------------------------------------------- */

/* `get-environment() -> list<tuple<string, string>>`. The canonical ABI
 * returns it through a caller-supplied area holding (ptr, len). Zero length is
 * an empty list and needs no allocation. */
void __wasm_import_environment_get_environment(int32_t retptr)
{
    uint32_t *ret = (uint32_t *)(uintptr_t)retptr;
    ret[0] = 0;
    ret[1] = 0;
}

/* Monotonic clock. */

/* Convert the host's coarse millisecond clock to WASI nanoseconds. The
 * host owns the clock sample for the serving slice. */
int64_t __wasm_import_monotonic_clock_now(void)
{
    return wisp_clock_now_coarse() * 1000000LL;
}

/* Wall clock. */

/* The canonical datetime result uses a 16-byte, 8-aligned area: seconds at
 * offset 0 and nanoseconds at offset 8. Both adapters derive from the host's
 * coarse millisecond clock. A serving slice observes its fixed clock sample;
 * a later slice can observe a later sample. */
void __wasm_import_wall_clock_now(int32_t retptr)
{
    int64_t ms = wisp_clock_now_coarse();
    if (ms < 0) {
        /* The host contract is Unix epoch milliseconds and cannot legitimately
         * be negative. Clamping rather than trusting it keeps the unsigned
         * division below from turning one bad host value into a wildly wrong
         * date the guest would then persist. */
        ms = 0;
    }
    uint64_t *seconds = (uint64_t *)(uintptr_t)retptr;
    uint32_t *nanoseconds = (uint32_t *)(uintptr_t)(retptr + 8);
    *seconds = (uint64_t)ms / 1000u;
    *nanoseconds = (uint32_t)((uint64_t)ms % 1000u) * 1000000u;
}

/* Entropy. */

/* `get-random-bytes(len: u64) -> list<u8>`. The callee allocates; wasi-libc
 * frees the result with `free`, so `malloc` is the correct allocator. This
 * path is NOT optional: .NET seeds object-header hash codes from `Random`,
 * and wasi-libc turns a failed draw into `_Exit`. */
void __wasm_import_random_get_random_bytes(int64_t len, int32_t retptr)
{
    uint32_t *ret = (uint32_t *)(uintptr_t)retptr;
    uint32_t n = (len < 0) ? 0u : (uint32_t)len;

    uint8_t *buf = (n == 0) ? NULL : (uint8_t *)malloc(n);
    if (n != 0 && buf == NULL) {
        static const char m[] = "wisp: out of memory drawing entropy";
        wisp_abort((const uint8_t *)m, (int32_t)(sizeof(m) - 1));
        __builtin_unreachable();
    }
    if (n != 0) {
        wisp_entropy_fill(buf, (int32_t)n);
    }
    ret[0] = (uint32_t)(uintptr_t)buf;
    ret[1] = n;
}

/* ---- dead: linked by the .NET PAL, never reached ----------------------- */
/* Verified unreachable by `guests/dotnet/check.sh --wasi=trap`. Kept as loud
 * aborts rather than removed, because "never reached" is a measurement about
 * one toolchain build, not a guarantee. */

/* (param i32) */
void __wasm_import_io_error_error_drop(int32_t a) { (void)a; WISP_PAL_DEAD("io/error drop"); }
void __wasm_import_poll_pollable_drop(int32_t a) { (void)a; WISP_PAL_DEAD("io/poll drop"); }
void __wasm_import_streams_input_stream_drop(int32_t a) { (void)a; WISP_PAL_DEAD("io/streams input drop"); }
void __wasm_import_streams_output_stream_drop(int32_t a) { (void)a; WISP_PAL_DEAD("io/streams output drop"); }
void __wasm_import_terminal_input_terminal_input_drop(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-input drop"); }
void __wasm_import_terminal_output_terminal_output_drop(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-output drop"); }
void __wasm_import_filesystem_descriptor_drop(int32_t a) { (void)a; WISP_PAL_DEAD("filesystem descriptor drop"); }
void __wasm_import_exit_exit(int32_t a) { (void)a; WISP_PAL_DEAD("cli/exit"); }
void __wasm_import_poll_method_pollable_block(int32_t a) { (void)a; WISP_PAL_DEAD("io/poll block"); }
void __wasm_import_terminal_stdin_get_terminal_stdin(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-stdin"); }
void __wasm_import_terminal_stdout_get_terminal_stdout(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-stdout"); }
void __wasm_import_terminal_stderr_get_terminal_stderr(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-stderr"); }
void __wasm_import_filesystem_preopens_get_directories(int32_t a) { (void)a; WISP_PAL_DEAD("filesystem/preopens"); }

/* (param i32) (result i32) */
int32_t __wasm_import_streams_method_output_stream_subscribe(int32_t a) { (void)a; WISP_PAL_DEAD("io/streams subscribe"); }

/* (param i32 i32) */
void __wasm_import_streams_method_output_stream_check_write(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("io/streams check-write"); }
void __wasm_import_streams_method_output_stream_blocking_flush(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("io/streams blocking-flush"); }
void __wasm_import_filesystem_method_descriptor_append_via_stream(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("filesystem append-via-stream"); }
void __wasm_import_filesystem_method_descriptor_get_flags(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("filesystem get-flags"); }
void __wasm_import_filesystem_method_descriptor_stat(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("filesystem stat"); }
void __wasm_import_filesystem_method_descriptor_metadata_hash(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("filesystem metadata-hash"); }

/* (param i32 i32 i32 i32) */
void __wasm_import_streams_method_output_stream_write(int32_t a, int32_t b, int32_t c, int32_t d) { (void)a; (void)b; (void)c; (void)d; WISP_PAL_DEAD("io/streams write"); }

/* (result i32) */
int32_t __wasm_import_stdin_get_stdin(void) { WISP_PAL_DEAD("cli/stdin"); }
int32_t __wasm_import_stdout_get_stdout(void) { WISP_PAL_DEAD("cli/stdout"); }
int32_t __wasm_import_stderr_get_stderr(void) { WISP_PAL_DEAD("cli/stderr"); }

/* (param i64) (result i32) */
int32_t __wasm_import_monotonic_clock_subscribe_instant(int64_t a) { (void)a; WISP_PAL_DEAD("monotonic subscribe-instant"); }
int32_t __wasm_import_monotonic_clock_subscribe_duration(int64_t a) { (void)a; WISP_PAL_DEAD("monotonic subscribe-duration"); }

/* (param i32 i64 i32) */
void __wasm_import_filesystem_method_descriptor_read_via_stream(int32_t a, int64_t b, int32_t c) { (void)a; (void)b; (void)c; WISP_PAL_DEAD("filesystem read-via-stream"); }
void __wasm_import_filesystem_method_descriptor_write_via_stream(int32_t a, int64_t b, int32_t c) { (void)a; (void)b; (void)c; WISP_PAL_DEAD("filesystem write-via-stream"); }
void __wasm_import_streams_method_input_stream_blocking_read(int32_t a, int64_t b, int32_t c) { (void)a; (void)b; (void)c; WISP_PAL_DEAD("io/streams blocking-read"); }

/* ---- dead: pulled in by the wider BCL surface -------------------------- */
/* Referencing LINQ, source-generated Regex or System.Text.Json makes the .NET
 * PAL link its directory and file-open paths, which adds the entries below.
 * None is reachable either, but the fact that the set GREW with the BCL surface
 * is the point: this file is a closure over what the tenant's code drags in,
 * not a fixed list, and `build.sh` fails the build if a new one appears rather
 * than letting it become an undeclared capability. */

/* (param i32) */
void __wasm_import_filesystem_directory_entry_stream_drop(int32_t a) { (void)a; WISP_PAL_DEAD("filesystem dir-entry-stream drop"); }

/* (param i32) (result i32) */
int32_t __wasm_import_streams_method_input_stream_subscribe(int32_t a) { (void)a; WISP_PAL_DEAD("io/streams input subscribe"); }

/* (param i32 i32) */
void __wasm_import_filesystem_method_descriptor_read_directory(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("filesystem read-directory"); }
void __wasm_import_filesystem_method_directory_entry_stream_read_directory_entry(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("filesystem read-directory-entry"); }

/* (param i32 i32 i32 i32) */
void __wasm_import_filesystem_method_descriptor_readlink_at(int32_t a, int32_t b, int32_t c, int32_t d) { (void)a; (void)b; (void)c; (void)d; WISP_PAL_DEAD("filesystem readlink-at"); }
void __wasm_import_filesystem_method_descriptor_unlink_file_at(int32_t a, int32_t b, int32_t c, int32_t d) { (void)a; (void)b; (void)c; (void)d; WISP_PAL_DEAD("filesystem unlink-file-at"); }

/* (param i32 i32 i32 i32 i32) */
void __wasm_import_filesystem_method_descriptor_stat_at(int32_t a, int32_t b, int32_t c, int32_t d, int32_t e) { (void)a; (void)b; (void)c; (void)d; (void)e; WISP_PAL_DEAD("filesystem stat-at"); }
void __wasm_import_filesystem_method_descriptor_metadata_hash_at(int32_t a, int32_t b, int32_t c, int32_t d, int32_t e) { (void)a; (void)b; (void)c; (void)d; (void)e; WISP_PAL_DEAD("filesystem metadata-hash-at"); }

/* (param i32 i32 i32 i32 i32 i32 i32) */
void __wasm_import_filesystem_method_descriptor_open_at(int32_t a, int32_t b, int32_t c, int32_t d, int32_t e, int32_t f, int32_t g) { (void)a; (void)b; (void)c; (void)d; (void)e; (void)f; (void)g; WISP_PAL_DEAD("filesystem open-at"); }

/* (param i32 i64 i64 i32 i32) */
void __wasm_import_filesystem_method_descriptor_advise(int32_t a, int64_t b, int64_t c, int32_t d, int32_t e) { (void)a; (void)b; (void)c; (void)d; (void)e; WISP_PAL_DEAD("filesystem advise"); }

/* (param i32 i64 i64 i32) */
void __wasm_import_filesystem_method_descriptor_read(int32_t a, int64_t b, int64_t c, int32_t d) { (void)a; (void)b; (void)c; (void)d; WISP_PAL_DEAD("filesystem read"); }
