#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

__attribute__((import_module("wisp"), import_name("log_emit")))
extern void wisp_debug_log_emit(int32_t, const char *, int32_t);
extern int __real_vfprintf(FILE *, const char *, va_list);
extern int __real_fputs(const char *, FILE *);

static int emit(const char *format, va_list arguments)
{
    va_list copy;
    va_copy(copy, arguments);
    int length = vsnprintf(NULL, 0, format, copy);
    va_end(copy);
    if (length < 0) return length;
    char *buffer = malloc((size_t)length + 1);
    if (!buffer) return -1;
    int written = vsnprintf(buffer, (size_t)length + 1, format, arguments);
    if (written >= 0) wisp_debug_log_emit(2, buffer, written);
    free(buffer);
    return written;
}

int __wrap_vfprintf(FILE *stream, const char *format, va_list arguments)
{
    // Formatting uses memory streams internally; only terminal output is a host log.
    if (stream != stdout && stream != stderr) return __real_vfprintf(stream, format, arguments);
    return emit(format, arguments);
}

int __wrap_vprintf(const char *format, va_list arguments)
{
    return emit(format, arguments);
}

int __wrap_printf(const char *format, ...)
{
    va_list arguments;
    va_start(arguments, format);
    int result = emit(format, arguments);
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
    wisp_debug_log_emit(2, text, (int32_t)strlen(text));
    return 0;
}

int __wrap_fputs(const char *text, FILE *stream)
{
    if (stream != stdout && stream != stderr) return __real_fputs(text, stream);
    wisp_debug_log_emit(2, text, (int32_t)strlen(text));
    return 0;
}
