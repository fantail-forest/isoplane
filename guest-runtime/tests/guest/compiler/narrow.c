#include <stdint.h>

#define PROBE(name, type, wide) \
    wide isoplane_probe_arg_##name(type value) { return value; } \
    type isoplane_probe_return_##name(wide value) { return (type)value; } \
    wide isoplane_probe_callback_##name(type (*callback)(type), type value) { return callback(value); }

PROBE(u8, uint8_t, uint32_t)
PROBE(i8, int8_t, int32_t)
PROBE(u16, uint16_t, uint32_t)
PROBE(i16, int16_t, int32_t)
