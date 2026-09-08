#include <wasi/wasip2.h>

int32_t wisp_dns_start(const uint8_t *name, int32_t length, int32_t *stream, int32_t *pollable) {
    instance_network_own_network_t network = instance_network_instance_network();
    wasip2_string_t text = {.ptr = (uint8_t *)name, .len = (size_t)length};
    ip_name_lookup_own_resolve_address_stream_t addresses;
    ip_name_lookup_error_code_t error;
    if (!ip_name_lookup_resolve_addresses((ip_name_lookup_borrow_network_t){network.__handle}, &text, &addresses, &error))
        return (int32_t)error + 1;
    *stream = addresses.__handle;
    *pollable = ip_name_lookup_method_resolve_address_stream_subscribe(
        ip_name_lookup_borrow_resolve_address_stream(addresses)).__handle;
    return 0;
}

int32_t wisp_dns_next(int32_t stream, uint8_t *bytes) {
    ip_name_lookup_option_ip_address_t address;
    ip_name_lookup_error_code_t error;
    if (!ip_name_lookup_method_resolve_address_stream_resolve_next_address(
            (ip_name_lookup_borrow_resolve_address_stream_t){stream}, &address, &error))
        return -((int32_t)error + 1);
    if (!address.is_some) return 0;
    if (address.val.tag == NETWORK_IP_ADDRESS_IPV4) {
        bytes[0] = address.val.val.ipv4.f0;
        bytes[1] = address.val.val.ipv4.f1;
        bytes[2] = address.val.val.ipv4.f2;
        bytes[3] = address.val.val.ipv4.f3;
        return 4;
    }
    uint16_t segments[] = {
        address.val.val.ipv6.f0, address.val.val.ipv6.f1,
        address.val.val.ipv6.f2, address.val.val.ipv6.f3,
        address.val.val.ipv6.f4, address.val.val.ipv6.f5,
        address.val.val.ipv6.f6, address.val.val.ipv6.f7
    };
    for (int index = 0; index < 8; ++index) {
        bytes[index * 2] = (uint8_t)(segments[index] >> 8);
        bytes[index * 2 + 1] = (uint8_t)segments[index];
    }
    return 16;
}

void wisp_dns_drop(int32_t stream) {
    ip_name_lookup_resolve_address_stream_drop_own(
        (ip_name_lookup_own_resolve_address_stream_t){stream});
}
