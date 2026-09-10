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
#include <fcntl.h>
#include <errno.h>
#include <stdlib.h>
#include <string.h>

/* ---- the WISP imports this PAL is built from --------------------------- */

__attribute__((import_module("wisp"), import_name("abort")))
extern void wisp_abort(const uint8_t *msg, int32_t len);

__attribute__((import_module("wisp"), import_name("entropy_fill")))
extern void wisp_entropy_fill(uint8_t *buf, int32_t len);

/* A dead PAL entry was reached: abort naming it. */
#define WISP_PAL_DEAD(name)                                                    \
    do {                                                                       \
        static const char m[] = "wisp: guest reached unsupported PAL entry "    \
                                name;                                          \
        wisp_abort((const uint8_t *)m, (int32_t)(sizeof(m) - 1));               \
        __builtin_unreachable();                                               \
    } while (0)

/* ---- live: environment -------------------------------------------------- */

/* `get-environment() -> list<tuple<string, string>>`, returned through a
 * caller-supplied (ptr, len) area. */
void __wasm_import_environment_get_environment(int32_t retptr)
{
    uint32_t *ret = (uint32_t *)(uintptr_t)retptr;
    ret[0] = 0;
    ret[1] = 0;
}

/* ---- live: entropy ------------------------------------------------------ */

/* `get-random-bytes(len: u64) -> list<u8>`. The callee allocates with
 * `malloc`; wasi-libc frees with `free`. .NET seeds object-header hash codes
 * from it during start-up, and wasi-libc turns a failed draw into `_Exit`. */
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

/* (param i32) */
void __wasm_import_terminal_input_terminal_input_drop(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-input drop"); }
void __wasm_import_terminal_output_terminal_output_drop(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-output drop"); }
void __wasm_import_exit_exit(int32_t a) { (void)a; WISP_PAL_DEAD("cli/exit"); }
void __wasm_import_terminal_stdin_get_terminal_stdin(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-stdin"); }
void __wasm_import_terminal_stdout_get_terminal_stdout(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-stdout"); }
void __wasm_import_terminal_stderr_get_terminal_stderr(int32_t a) { (void)a; WISP_PAL_DEAD("cli/terminal-stderr"); }

/* (param i32) (result i32) */

/* (param i32 i32) */
void __wasm_import_streams_method_output_stream_blocking_flush(int32_t a, int32_t b) { (void)a; (void)b; WISP_PAL_DEAD("io/streams blocking-flush"); }

/* (param i32 i32 i32 i32) */

/* (result i32) */
int32_t __wasm_import_stdin_get_stdin(void) { WISP_PAL_DEAD("cli/stdin"); }
int32_t __wasm_import_stdout_get_stdout(void) { WISP_PAL_DEAD("cli/stdout"); }
int32_t __wasm_import_stderr_get_stderr(void) { WISP_PAL_DEAD("cli/stderr"); }

/* (param i64) (result i32) */

/* (param i32 i64 i32) */
void __wasm_import_streams_method_input_stream_blocking_read(int32_t a, int64_t b, int32_t c) { (void)a; (void)b; (void)c; WISP_PAL_DEAD("io/streams blocking-read"); }

/* ---- dead: pulled in by LINQ, Regex and System.Text.Json --------------- */

/* (param i32) */

/* (param i32) (result i32) */

/* (param i32 i32) */

/* (param i32 i32 i32 i32) */

/* (param i32 i32 i32 i32 i32) */

/* (param i32 i32 i32 i32 i32 i32 i32) */

/* (param i32 i64 i64 i32 i32) */

/* (param i32 i64 i64 i32) */

#include "wisp_filesystem.h"

/* The managed WASI shim fails without setting errno. The empty disk denies
 * creation before reserving a name; open routes that request through the host
 * adapter and preserves its error for the managed IOException. */
intptr_t __wrap_SystemNative_MksTemps(char *path_template, int32_t suffix_length)
{
    size_t length = strlen(path_template);
    if (suffix_length < 0 || (size_t)suffix_length > length ||
        length - (size_t)suffix_length < 6 ||
        memcmp(path_template + length - (size_t)suffix_length - 6, "XXXXXX", 6) != 0) {
        errno = EINVAL;
        return -1;
    }
    return open(path_template, O_RDWR | O_CREAT | O_EXCL, 0600);
}

#include "wisp_socket_poll.h"
static void wisp_capture_socket(const uint8_t *result);
#include "wisp_native_io.h"
#include "wisp_socket_send.h"
#include "wisp_socket_options.h"
