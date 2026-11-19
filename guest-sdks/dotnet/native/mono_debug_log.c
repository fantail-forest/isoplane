#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

// The debug runtime's native code writes to the console directly, where the
// production runtime's goes through Mono's print handlers. The linker points
// those calls here, and they become the same host log records. fwrite is
// among them because the compiler turns fputs and fprintf of a constant
// string into it, and an fwrite that reached the console would end the
// instance: the host serves no console.
extern void mono_runtime_log(int32_t, const char *);
extern int __real_vfprintf(FILE *, const char *, va_list);
extern int __real_fputs(const char *, FILE *);
extern size_t __real_fwrite(const void *, size_t, size_t, FILE *);

static int32_t level_of(FILE *stream)
{
    return stream == stderr ? 3 : 2;
}

static int emit(FILE *stream, const char *format, va_list arguments)
{
    va_list copy;
    va_copy(copy, arguments);
    int length = vsnprintf(NULL, 0, format, copy);
    va_end(copy);
    if (length < 0) return length;
    char *buffer = malloc((size_t)length + 1);
    if (!buffer) return -1;
    int written = vsnprintf(buffer, (size_t)length + 1, format, arguments);
    if (written >= 0) mono_runtime_log(level_of(stream), buffer);
    free(buffer);
    return written;
}

int __wrap_vfprintf(FILE *stream, const char *format, va_list arguments)
{
    // Formatting uses memory streams internally; only terminal output is a host log.
    if (stream != stdout && stream != stderr) return __real_vfprintf(stream, format, arguments);
    return emit(stream, format, arguments);
}

int __wrap_vprintf(const char *format, va_list arguments)
{
    return emit(stdout, format, arguments);
}

int __wrap_printf(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int result = emit(stdout, format, arguments);
    va_end(arguments);
    return result;
}

int __wrap_fprintf(FILE *stream, const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int result = __wrap_vfprintf(stream, format, arguments);
    va_end(arguments);
    return result;
}

int __wrap_puts(const char *text)
{
    mono_runtime_log(level_of(stdout), text);
    return 0;
}

int __wrap_fputs(const char *text, FILE *stream)
{
    if (stream != stdout && stream != stderr) return __real_fputs(text, stream);
    mono_runtime_log(level_of(stream), text);
    return 0;
}

size_t __wrap_fwrite(const void *data, size_t size, size_t count, FILE *stream)
{
    if (stream != stdout && stream != stderr) return __real_fwrite(data, size, count, stream);
    size_t length = size * count;
    char *text = malloc(length + 1);
    if (!text) return 0;
    memcpy(text, data, length);
    text[length] = 0;
    mono_runtime_log(level_of(stream), text);
    free(text);
    return count;
}
