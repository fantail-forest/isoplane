// Only the local debugger host may wait while a slice is running, and only a
// local debug session has a console: the runtime's log. wasi-libc's write
// reaches standard output and standard error through the streams these
// functions return, so every console write arrives here whichever libc call
// made it.
#define __wasm_import_poll_method_pollable_block isoplane_unsupported_pollable_block
#define __wasm_import_stdout_get_stdout isoplane_unsupported_stdout
#define __wasm_import_stderr_get_stderr isoplane_unsupported_stderr
#define __wasm_import_terminal_stdout_get_terminal_stdout isoplane_unsupported_terminal_stdout
#define __wasm_import_terminal_stderr_get_terminal_stderr isoplane_unsupported_terminal_stderr
#define __wasm_import_terminal_output_terminal_output_drop isoplane_unsupported_terminal_output_drop
#define __wasm_import_streams_method_output_stream_blocking_flush isoplane_unsupported_blocking_flush
#define __wasm_import_streams_method_output_stream_subscribe isoplane_socket_output_subscribe
#define __wasm_import_streams_method_output_stream_check_write isoplane_socket_check_write
#define __wasm_import_streams_method_output_stream_write isoplane_socket_write
#define __wasm_import_streams_output_stream_drop isoplane_socket_output_drop
#include "isoplane_pal.c"
#undef __wasm_import_poll_method_pollable_block
#undef __wasm_import_stdout_get_stdout
#undef __wasm_import_stderr_get_stderr
#undef __wasm_import_terminal_stdout_get_terminal_stdout
#undef __wasm_import_terminal_stderr_get_terminal_stderr
#undef __wasm_import_terminal_output_terminal_output_drop
#undef __wasm_import_streams_method_output_stream_blocking_flush
#undef __wasm_import_streams_method_output_stream_subscribe
#undef __wasm_import_streams_method_output_stream_check_write
#undef __wasm_import_streams_method_output_stream_write
#undef __wasm_import_streams_output_stream_drop

// The sink of Mono's print handlers in mono_entry.c.
extern void mono_runtime_log(int32_t, const char *, size_t);

void __wasm_import_poll_method_pollable_block(int32_t handle)
{
    uint32_t result[2];
    __wasm_import_poll_poll((int32_t)(uintptr_t)&handle, 1, (int32_t)(uintptr_t)result);
    free((void *)(uintptr_t)result[0]);
}

// The host numbers its resources upward from 1 and never issues UINT32_MAX,
// so a host stream could take the standard error handle only after four
// billion others.
static const int32_t isoplane_console_stdout = (int32_t)UINT32_MAX;
static const int32_t isoplane_console_stderr = (int32_t)(UINT32_MAX - 1);

// The log level of a console stream, or -1 for a host stream.
static int32_t isoplane_console_level(int32_t stream)
{
    if (stream == isoplane_console_stdout) return 2;
    if (stream == isoplane_console_stderr) return 3;
    return -1;
}

int32_t __wasm_import_stdout_get_stdout(void) { return isoplane_console_stdout; }
int32_t __wasm_import_stderr_get_stderr(void) { return isoplane_console_stderr; }

// A terminal, so that libc flushes standard output at each line ending
// rather than when its buffer fills. The host has no terminals of its own.
static void isoplane_console_terminal(int32_t stream, int32_t retptr)
{
    uint8_t *ret = (uint8_t *)(uintptr_t)retptr;
    ret[0] = 1;
    *(int32_t *)(ret + 4) = stream;
}

void __wasm_import_terminal_stdout_get_terminal_stdout(int32_t retptr) { isoplane_console_terminal(isoplane_console_stdout, retptr); }
void __wasm_import_terminal_stderr_get_terminal_stderr(int32_t retptr) { isoplane_console_terminal(isoplane_console_stderr, retptr); }
void __wasm_import_terminal_output_terminal_output_drop(int32_t terminal) { (void)terminal; }

// A console write never waits, so its stream's pollable is a timer that is
// already ready.
int32_t __wasm_import_streams_method_output_stream_subscribe(int32_t stream)
{
    if (isoplane_console_level(stream) < 0) return isoplane_socket_output_subscribe(stream);
    return __wasm_import_monotonic_clock_subscribe_duration(0);
}

void __wasm_import_streams_method_output_stream_check_write(int32_t stream, int32_t retptr)
{
    if (isoplane_console_level(stream) < 0) {
        isoplane_socket_check_write(stream, retptr);
        return;
    }
    uint8_t *ret = (uint8_t *)(uintptr_t)retptr;
    ret[0] = 0;
    *(uint64_t *)(ret + 8) = UINT64_MAX;
}

void __wasm_import_streams_method_output_stream_write(int32_t stream, int32_t data, int32_t length, int32_t retptr)
{
    int32_t level = isoplane_console_level(stream);
    if (level < 0) {
        isoplane_socket_write(stream, data, length, retptr);
        return;
    }
    mono_runtime_log(level, (const char *)(uintptr_t)data, (uint32_t)length);
    *(uint8_t *)(uintptr_t)retptr = 0;
}

void __wasm_import_streams_method_output_stream_blocking_flush(int32_t stream, int32_t retptr)
{
    if (isoplane_console_level(stream) < 0) {
        isoplane_unsupported_blocking_flush(stream, retptr);
        return;
    }
    *(uint8_t *)(uintptr_t)retptr = 0;
}

void __wasm_import_streams_output_stream_drop(int32_t stream)
{
    if (isoplane_console_level(stream) < 0) isoplane_socket_output_drop(stream);
}
