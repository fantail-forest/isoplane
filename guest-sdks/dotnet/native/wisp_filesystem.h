/* Generated from wisp_abi::filesystem::IMPORTS. */
__attribute__((import_module("wisp"), import_name("filesystem_call")))
extern void wisp_filesystem_call(int32_t operation, const uint64_t *arguments, int32_t count);
__attribute__((export_name("wisp_fs_alloc")))
uint32_t wisp_fs_alloc(uint32_t size) { return (uint32_t)(uintptr_t)malloc(size); }
__attribute__((export_name("wisp_fs_free")))
void wisp_fs_free(uint32_t pointer) { free((void *)(uintptr_t)pointer); }
void __wasm_import_filesystem_preopens_get_directories(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; wisp_filesystem_call(0, args, 1); }
void __wasm_import_filesystem_descriptor_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; wisp_filesystem_call(1, args, 1); }
void __wasm_import_filesystem_directory_entry_stream_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; wisp_filesystem_call(2, args, 1); }
void __wasm_import_filesystem_method_descriptor_read_via_stream(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; wisp_filesystem_call(3, args, 3); }
void __wasm_import_filesystem_method_descriptor_write_via_stream(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; wisp_filesystem_call(4, args, 3); }
void __wasm_import_filesystem_method_descriptor_append_via_stream(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; wisp_filesystem_call(5, args, 2); }
void __wasm_import_filesystem_method_descriptor_advise(int32_t a0, int64_t a1, int64_t a2, int32_t a3, int32_t a4) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint64_t)a2, (uint32_t)a3, (uint32_t)a4 }; wisp_filesystem_call(6, args, 5); }
void __wasm_import_filesystem_method_descriptor_get_flags(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; wisp_filesystem_call(7, args, 2); }
void __wasm_import_filesystem_method_descriptor_get_type(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; wisp_filesystem_call(8, args, 2); }
void __wasm_import_filesystem_method_descriptor_read(int32_t a0, int64_t a1, int64_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint64_t)a2, (uint32_t)a3 }; wisp_filesystem_call(9, args, 4); }
void __wasm_import_filesystem_method_descriptor_read_directory(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; wisp_filesystem_call(10, args, 2); }
void __wasm_import_filesystem_method_descriptor_stat(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; wisp_filesystem_call(11, args, 2); }
void __wasm_import_filesystem_method_descriptor_stat_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4 }; wisp_filesystem_call(12, args, 5); }
void __wasm_import_filesystem_method_descriptor_open_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4, (uint32_t)a5, (uint32_t)a6 }; wisp_filesystem_call(13, args, 7); }
void __wasm_import_filesystem_method_descriptor_readlink_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; wisp_filesystem_call(14, args, 4); }
void __wasm_import_filesystem_method_descriptor_unlink_file_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; wisp_filesystem_call(15, args, 4); }
void __wasm_import_filesystem_method_descriptor_metadata_hash(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; wisp_filesystem_call(16, args, 2); }
void __wasm_import_filesystem_method_descriptor_metadata_hash_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4 }; wisp_filesystem_call(17, args, 5); }
void __wasm_import_filesystem_method_directory_entry_stream_read_directory_entry(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; wisp_filesystem_call(18, args, 2); }
void __wasm_import_filesystem_method_descriptor_set_size(int32_t a0, int64_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint64_t)a1, (uint32_t)a2 }; wisp_filesystem_call(19, args, 3); }
void __wasm_import_filesystem_method_descriptor_write(int32_t a0, int32_t a1, int32_t a2, int64_t a3, int32_t a4) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint64_t)a3, (uint32_t)a4 }; wisp_filesystem_call(20, args, 5); }
void __wasm_import_filesystem_method_descriptor_sync(int32_t a0, int32_t a1) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1 }; wisp_filesystem_call(21, args, 2); }
void __wasm_import_filesystem_method_descriptor_create_directory_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; wisp_filesystem_call(22, args, 4); }
void __wasm_import_filesystem_method_descriptor_remove_directory_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3 }; wisp_filesystem_call(23, args, 4); }
void __wasm_import_filesystem_method_descriptor_rename_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4, (uint32_t)a5, (uint32_t)a6 }; wisp_filesystem_call(24, args, 7); }
void __wasm_import_filesystem_method_descriptor_symlink_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4, (uint32_t)a5 }; wisp_filesystem_call(25, args, 6); }
void __wasm_import_filesystem_method_descriptor_set_times(int32_t a0, int32_t a1, int64_t a2, int32_t a3, int32_t a4, int64_t a5, int32_t a6, int32_t a7) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint64_t)a2, (uint32_t)a3, (uint32_t)a4, (uint64_t)a5, (uint32_t)a6, (uint32_t)a7 }; wisp_filesystem_call(26, args, 8); }
void __wasm_import_filesystem_method_descriptor_link_at(int32_t a0, int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6, int32_t a7) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2, (uint32_t)a3, (uint32_t)a4, (uint32_t)a5, (uint32_t)a6, (uint32_t)a7 }; wisp_filesystem_call(27, args, 8); }
