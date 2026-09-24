#include "mono_assembly.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
extern void __wasm_call_ctors(void);
extern int initialize_runtime(void);
extern void *mono_wasm_assembly_load(const char *);
extern void *mono_wasm_assembly_find_class(void *,const char *, const char *);
extern void *mono_wasm_assembly_find_method(void *,const char *, int);
extern void *mono_runtime_invoke(void *, void *, void **, void **);
extern void *mono_object_unbox(void *);
extern void *mono_object_get_class(void *);
extern const char *mono_class_get_name(void *);
extern const char *mono_class_get_namespace(void *);
__attribute__((import_module("isoplane"), import_name("abort"))) extern void host_abort(uint8_t *, int32_t);
__attribute__((import_module("isoplane"), import_name("log_emit"))) extern void host_log(int32_t, const char *, int32_t);
typedef void (*MonoLogCallback)(const char *, const char *, const char *, int32_t, void *);
typedef void (*MonoPrintCallback)(const char *, int32_t);
extern void mono_trace_set_log_handler(MonoLogCallback, void *);
extern void mono_trace_set_print_handler(MonoPrintCallback);
extern void mono_trace_set_printerr_handler(MonoPrintCallback);
#ifdef ISOPLANE_LOCAL_DEBUG
extern void mono_wasm_enable_debugging(int);
extern int mono_jit_parse_options(int, char **);
extern int mono_wasm_add_assembly(const char *, const unsigned char *, unsigned int);
#include "mono_debug.h"
#endif
static void *klass;
static void fail(const char *message)
{
    host_abort((uint8_t *)message, (int32_t)strlen(message));
    __builtin_trap();
}
// The host log's levels, trace 0 to error 4.
static int32_t log_level(const char *level)
{
    if (!level) return 2;
    if (strcmp(level, "error") == 0 || strcmp(level, "critical") == 0) return 4;
    if (strcmp(level, "warning") == 0) return 3;
    if (strcmp(level, "debug") == 0) return 1;
    return 2;
}

static void emit(int32_t level, const char *text)
{
    size_t length = strlen(text);
    while (length > 0 && text[length - 1] == '\n') length--;
    if (length > 0) host_log(level, text, (int32_t)length);
}

// The runtime's own diagnostics go to the guest log; a fatal one ends the
// instance with its message, since Mono expects no return from it.
static void on_log(const char *domain, const char *level, const char *message, int32_t fatal, void *user_data)
{
    (void)domain;
    (void)user_data;
    if (fatal) fail(message);
    emit(log_level(level), message);
}

static void on_print(const char *text, int32_t is_stdout)
{
    emit(is_stdout ? 2 : 3, text);
}

#ifdef ISOPLANE_LOCAL_DEBUG
#define ISOPLANE_DEBUG_AGENT_PATH "/.isoplane/debug-agent"

// The session's debugger endpoint, which the host serves only to a local
// debug session. Only a loopback port is taken from it, so the file cannot
// choose any other agent option.
static void debug_agent_options(char *options, size_t size)
{
    int file = open(ISOPLANE_DEBUG_AGENT_PATH, O_RDONLY);
    if (file < 0) fail("local debugging requires the debug session the host serves");
    char endpoint[32] = {0};
    // A positioned read is a single descriptor read; a plain read goes through
    // a stream the PAL serves without blocking, which can return nothing yet.
    ssize_t read_bytes = pread(file, endpoint, sizeof(endpoint) - 1, 0);
    close(file);
    size_t length = read_bytes < 0 ? 0 : (size_t)read_bytes;
    static const char prefix[] = "127.0.0.1:";
    size_t start = sizeof(prefix) - 1;
    unsigned long port = 0;
    int valid = length > start && length - start <= 5 && memcmp(endpoint, prefix, start) == 0;
    for (size_t index = start; valid && index < length; index++) {
        valid = endpoint[index] >= '0' && endpoint[index] <= '9';
        port = port * 10 + (unsigned long)(endpoint[index] - '0');
    }
    if (!valid || port < 1 || port > 65535) {
        for (size_t index = 0; index < length; index++)
            if (endpoint[index] < ' ' || endpoint[index] > '~') endpoint[index] = '?';
        char message[96];
        snprintf(message, sizeof(message), "the debug session must name 127.0.0.1:<port> from 1 to 65535, not '%s'", endpoint);
        fail(message);
    }
    snprintf(options, size, "--debugger-agent=transport=dt_socket,address=127.0.0.1:%lu,server=n,suspend=y", port);
}
#endif

static void boot(void)
{
    if (klass) return;
    __wasm_call_ctors();
    // The host serves no console, so runtime output goes to the guest log.
    mono_trace_set_print_handler(on_print);
    mono_trace_set_printerr_handler(on_print);
#ifdef ISOPLANE_LOCAL_DEBUG
    mono_wasm_enable_debugging(1);
    char agent[96];
    debug_agent_options(agent, sizeof(agent));
    char *options[] = {agent};
    mono_jit_parse_options(1, options);
#endif
    if (initialize_runtime() != 0) fail("Mono initialization failed");
    // Start-up installs the WASI driver's logger, which writes to the console.
    mono_trace_set_log_handler(on_log, 0);
#ifdef ISOPLANE_LOCAL_DEBUG
    // Bundle initialization must precede registering symbols for its assembly.
    isoplane_register_debug_symbols();
#endif
    void *assembly = mono_wasm_assembly_load(ISOPLANE_MONO_ASSEMBLY);
    if (!assembly) fail("Managed IL app assembly missing");
    klass = mono_wasm_assembly_find_class(assembly, "", "IsoplaneGeneratedExports");
    if (!klass) fail("Managed IL entry class missing");
}

static void *invoke(const char *name, int count, void **arguments)
{
    boot();
    void *method = mono_wasm_assembly_find_method(klass, name, count);
    if (!method) fail("Managed IL entry method missing");
    void *exception = 0;
    void *result = mono_runtime_invoke(method, 0, arguments, &exception);
    if (exception) {
        void *exception_class = mono_object_get_class(exception);
        char message[256];
        snprintf(message, sizeof(message), "Mono %s failed: %s.%s", name,
                 mono_class_get_namespace(exception_class), mono_class_get_name(exception_class));
        fail(message);
    }
    return result;
}

int32_t isoplane_alloc(int32_t size, int32_t align)
{
    void *args[] = {&size, &align};
    void *value = invoke("Alloc", 2, args);
    return *(int32_t *)mono_object_unbox(value);
}

int64_t isoplane_handle(int32_t pointer, int32_t length)
{
    void *args[] = {&pointer, &length};
    void *value = invoke("Handle", 2, args);
    return *(int64_t *)mono_object_unbox(value);
}

int64_t isoplane_wake(int64_t future, int32_t pointer, int32_t length)
{
    void *args[] = {&future, &pointer, &length};
    void *value = invoke("Wake", 3, args);
    return *(int64_t *)mono_object_unbox(value);
}

int64_t isoplane_initialize(void)
{
    void *value = invoke("Initialize", 0, 0);
    return *(int64_t *)mono_object_unbox(value);
}

int64_t isoplane_initialize_runtime(void)
{
    void *value = invoke("InitializeRuntime", 0, 0);
    return *(int64_t *)mono_object_unbox(value);
}

void isoplane_gc_hint(int32_t pressure)
{
    void *args[] = {&pressure};
    invoke("GcHint", 1, args);
}
