/* Generated from isoplane_abi::native_io::canonical. */
#define ISOPLANE_SEND_PENDING UINT64_C(18446744073709551615)
#define ISOPLANE_SEND_FAILED UINT64_C(18446744073709551614)
#define ISOPLANE_SEND_IS_FAILURE(result) ((uint64_t)(result) <= ISOPLANE_SEND_FAILED && (uint64_t)(result) > ISOPLANE_SEND_FAILED - UINT64_C(64))
#define ISOPLANE_SEND_FAILURE_CODE(result) ((uint8_t)(ISOPLANE_SEND_FAILED - (uint64_t)(result)))
__attribute__((import_module("isoplane"), import_name("socket_call")))
extern int64_t isoplane_socket_call(int32_t operation, const uint64_t *arguments, int32_t count);
__attribute__((import_module("isoplane"), import_name("poll_call")))
extern int64_t isoplane_poll_call(int32_t operation, const uint64_t *arguments, int32_t count);
int32_t __wasm_import_monotonic_clock_subscribe_duration(int64_t a0) { const uint64_t args[] = { (uint64_t)a0 }; return (int32_t)isoplane_poll_call(0, args, 1); }
void __wasm_import_poll_pollable_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_forget_pollable((uint32_t)a0); isoplane_poll_call(1, args, 1); }
void __wasm_import_poll_poll(int32_t a0, int32_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2 }; isoplane_capture_pollables((const uint32_t *)(uintptr_t)a0, (uint32_t)a1); isoplane_poll_call(2, args, 3); }
void __wasm_import_udp_udp_socket_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_socket_call(3, args, 1); }
void __wasm_import_udp_incoming_datagram_stream_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_socket_call(4, args, 1); }
void __wasm_import_udp_outgoing_datagram_stream_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_socket_call(5, args, 1); }
void __wasm_import_tcp_tcp_socket_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_socket_call(6, args, 1); }
void __wasm_import_streams_method_input_stream_read(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(7, args, 3); }
int32_t __wasm_import_instance_network_instance_network(void) { const uint64_t args[] = { 0 }; return (int32_t)isoplane_socket_call(8, args, 0); }
void __wasm_import_udp_method_udp_socket_start_bind(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6, int32_t a7, int32_t a8, int32_t a9, int32_t a10, int32_t a11, int32_t a12, int32_t a13, int32_t a14) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4, (uint32_t)a5, (uint32_t)a6, (uint32_t)a7, (uint32_t)a8, (uint32_t)a9, (uint32_t)a10, (uint32_t)a11, (uint32_t)a12, (uint32_t)a13, (uint32_t)a14 }; isoplane_socket_call(9, args, 15); }
void __wasm_import_udp_method_udp_socket_finish_bind(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(10, args, 2); }
void __wasm_import_udp_method_udp_socket_stream(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6, int32_t a7, int32_t a8, int32_t a9, int32_t a10, int32_t a11, int32_t a12, int32_t a13, int32_t a14) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4, (uint32_t)a5, (uint32_t)a6, (uint32_t)a7, (uint32_t)a8, (uint32_t)a9, (uint32_t)a10, (uint32_t)a11, (uint32_t)a12, (uint32_t)a13, (uint32_t)a14 }; isoplane_socket_call(11, args, 15); }
void __wasm_import_udp_method_udp_socket_local_address(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(12, args, 2); }
void __wasm_import_udp_method_udp_socket_remote_address(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(13, args, 2); }
void __wasm_import_udp_method_udp_socket_unicast_hop_limit(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(14, args, 2); }
void __wasm_import_udp_method_udp_socket_set_unicast_hop_limit(int32_t a0, int32_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2 }; isoplane_socket_call(15, args, 3); }
void __wasm_import_udp_method_udp_socket_receive_buffer_size(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(16, args, 2); }
void __wasm_import_udp_method_udp_socket_set_receive_buffer_size(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(17, args, 3); }
void __wasm_import_udp_method_udp_socket_send_buffer_size(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(18, args, 2); }
void __wasm_import_udp_method_udp_socket_set_send_buffer_size(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(19, args, 3); }
int32_t __wasm_import_udp_method_udp_socket_subscribe(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_socket_call(20, args, 1); }
void __wasm_import_udp_method_incoming_datagram_stream_receive(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(21, args, 3); }
int32_t __wasm_import_udp_method_incoming_datagram_stream_subscribe(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_socket_call(22, args, 1); }
void __wasm_import_udp_method_outgoing_datagram_stream_check_send(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(23, args, 2); }
void __wasm_import_udp_method_outgoing_datagram_stream_send(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; isoplane_socket_call(24, args, 4); }
int32_t __wasm_import_udp_method_outgoing_datagram_stream_subscribe(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_socket_call(25, args, 1); }
void __wasm_import_udp_create_socket_create_udp_socket(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(26, args, 2); }
void __wasm_import_tcp_method_tcp_socket_start_bind(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6, int32_t a7, int32_t a8, int32_t a9, int32_t a10, int32_t a11, int32_t a12, int32_t a13, int32_t a14) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4, (uint32_t)a5, (uint32_t)a6, (uint32_t)a7, (uint32_t)a8, (uint32_t)a9, (uint32_t)a10, (uint32_t)a11, (uint32_t)a12, (uint32_t)a13, (uint32_t)a14 }; isoplane_socket_call(27, args, 15); }
void __wasm_import_tcp_method_tcp_socket_finish_bind(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(28, args, 2); }
void __wasm_import_tcp_method_tcp_socket_start_connect(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6, int32_t a7, int32_t a8, int32_t a9, int32_t a10, int32_t a11, int32_t a12, int32_t a13, int32_t a14) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4, (uint32_t)a5, (uint32_t)a6, (uint32_t)a7, (uint32_t)a8, (uint32_t)a9, (uint32_t)a10, (uint32_t)a11, (uint32_t)a12, (uint32_t)a13, (uint32_t)a14 }; isoplane_socket_call(29, args, 15); }
void __wasm_import_tcp_method_tcp_socket_finish_connect(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(30, args, 2); }
void __wasm_import_tcp_method_tcp_socket_start_listen(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(31, args, 2); }
void __wasm_import_tcp_method_tcp_socket_finish_listen(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(32, args, 2); }
void __wasm_import_tcp_method_tcp_socket_accept(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(33, args, 2); }
void __wasm_import_tcp_method_tcp_socket_local_address(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(34, args, 2); }
void __wasm_import_tcp_method_tcp_socket_remote_address(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(35, args, 2); }
int32_t __wasm_import_tcp_method_tcp_socket_is_listening(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_socket_call(36, args, 1); }
void __wasm_import_tcp_method_tcp_socket_set_listen_backlog_size(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(37, args, 3); }
void __wasm_import_tcp_method_tcp_socket_keep_alive_enabled(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(38, args, 2); }
void __wasm_import_tcp_method_tcp_socket_set_keep_alive_enabled(int32_t a0, int32_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2 }; isoplane_socket_call(39, args, 3); }
void __wasm_import_tcp_method_tcp_socket_keep_alive_idle_time(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(40, args, 2); }
void __wasm_import_tcp_method_tcp_socket_set_keep_alive_idle_time(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(41, args, 3); }
void __wasm_import_tcp_method_tcp_socket_keep_alive_interval(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(42, args, 2); }
void __wasm_import_tcp_method_tcp_socket_set_keep_alive_interval(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(43, args, 3); }
void __wasm_import_tcp_method_tcp_socket_keep_alive_count(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(44, args, 2); }
void __wasm_import_tcp_method_tcp_socket_set_keep_alive_count(int32_t a0, int32_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2 }; isoplane_socket_call(45, args, 3); }
void __wasm_import_tcp_method_tcp_socket_hop_limit(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(46, args, 2); }
void __wasm_import_tcp_method_tcp_socket_set_hop_limit(int32_t a0, int32_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2 }; isoplane_socket_call(47, args, 3); }
void __wasm_import_tcp_method_tcp_socket_receive_buffer_size(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(48, args, 2); }
void __wasm_import_tcp_method_tcp_socket_set_receive_buffer_size(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(49, args, 3); }
void __wasm_import_tcp_method_tcp_socket_send_buffer_size(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(50, args, 2); }
void __wasm_import_tcp_method_tcp_socket_set_send_buffer_size(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; isoplane_socket_call(51, args, 3); }
int32_t __wasm_import_tcp_method_tcp_socket_subscribe(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_socket_call(52, args, 1); }
void __wasm_import_tcp_method_tcp_socket_shutdown(int32_t a0, int32_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2 }; isoplane_socket_call(53, args, 3); }
void __wasm_import_tcp_create_socket_create_tcp_socket(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(54, args, 2); isoplane_capture_socket((const uint8_t *)(uintptr_t)a1); }
int32_t __wasm_import_monotonic_clock_subscribe_instant(int64_t a0) { const uint64_t args[] = { (uint64_t)a0 }; return (int32_t)isoplane_poll_call(57, args, 1); }
int32_t __wasm_import_poll_method_pollable_ready(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_poll_call(58, args, 1); }
void __wasm_import_poll_method_pollable_block(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_poll_call(59, args, 1); }
void __wasm_import_streams_input_stream_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_socket_call(60, args, 1); }
void __wasm_import_streams_output_stream_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_socket_call(61, args, 1); }
int32_t __wasm_import_streams_method_input_stream_subscribe(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_socket_call(62, args, 1); }
int32_t __wasm_import_streams_method_output_stream_subscribe(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_socket_call(63, args, 1); }
void __wasm_import_streams_method_output_stream_check_write(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(64, args, 2); }
void __wasm_import_streams_method_output_stream_write(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; isoplane_socket_call(65, args, 4); }
void __wasm_import_streams_method_output_stream_flush(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(66, args, 2); }
void __wasm_import_io_error_error_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_socket_call(67, args, 1); }
void __wasm_import_io_error_method_error_to_debug_string(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(68, args, 2); }
int64_t __wasm_import_isoplane_socket_try_send(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; return (int64_t)isoplane_socket_call(69, args, 4); }
void __wasm_import_ip_name_lookup_resolve_addresses(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; isoplane_socket_call(70, args, 4); }
void __wasm_import_ip_name_lookup_method_resolve_address_stream_resolve_next_address(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(71, args, 2); }
int32_t __wasm_import_ip_name_lookup_method_resolve_address_stream_subscribe(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)isoplane_socket_call(72, args, 1); }
void __wasm_import_ip_name_lookup_resolve_address_stream_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; isoplane_socket_call(73, args, 1); }
int32_t __wasm_import_isoplane_socket_no_delay(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; return (int32_t)isoplane_socket_call(74, args, 2); }
void __wasm_import_network_network_error_code(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; isoplane_socket_call(75, args, 2); }
int64_t __wasm_import_isoplane_socket_try_send_with_error_code(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; return (int64_t)isoplane_socket_call(76, args, 4); }
