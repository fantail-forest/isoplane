#include "mono_assembly.h"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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
__attribute__((import_module("wisp"), import_name("abort"))) extern void host_abort(uint8_t *, int32_t);
static void *klass;
static void fail(const char *message)
{
    host_abort((uint8_t *)message, (int32_t)strlen(message));
    __builtin_trap();
}

static void boot(void)
{
    if (klass) return;
    __wasm_call_ctors();
    if (initialize_runtime() != 0) fail("Mono initialization failed");
    void *assembly = mono_wasm_assembly_load(WISP_MONO_ASSEMBLY);
    if (!assembly) fail("Managed IL app assembly missing");
    klass = mono_wasm_assembly_find_class(assembly, "", "WispGeneratedExports");
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

int32_t wisp_alloc(int32_t size, int32_t align)
{
    void *args[] = {&size, &align};
    void *value = invoke("Alloc", 2, args);
    return *(int32_t *)mono_object_unbox(value);
}

int64_t wisp_handle(int32_t pointer, int32_t length)
{
    void *args[] = {&pointer, &length};
    void *value = invoke("Handle", 2, args);
    return *(int64_t *)mono_object_unbox(value);
}

int64_t wisp_wake(int64_t future, int32_t pointer, int32_t length)
{
    void *args[] = {&future, &pointer, &length};
    void *value = invoke("Wake", 3, args);
    return *(int64_t *)mono_object_unbox(value);
}

void wisp_initialize(void)
{
    invoke("Initialize", 0, 0);
}

void wisp_gc_hint(int32_t pressure)
{
    void *args[] = {&pressure};
    invoke("GcHint", 1, args);
}
