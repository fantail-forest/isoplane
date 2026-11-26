use crate::imports::{Sig, Ty};

/// The directory the host reserves for files it serves on an application's
/// behalf. None of the application's own content is served beneath it.
pub const HOST_ROOT: &str = "/.isoplane";

/// Where the host serves an interpreted application's package, its
/// `application.json` and `assemblies/` laid out as in the managed bundle,
/// for the shared Mono runtime's loader to read during initialization.
pub const APPLICATION_PACKAGE_ROOT: &str = "/.isoplane/package";

/// Where the host serves a local debug session's debugger endpoint,
/// `127.0.0.1:<port>`, for a debug runtime to connect its agent to. Only a
/// local debug session serves it, so no prepared template captures it.
pub const DEBUG_AGENT_PATH: &str = "/.isoplane/debug-agent";

/// The only Isoplane entry used by the native filesystem PAL wrappers.
pub const IMPORT: &str = "filesystem_call";

/// An operation with its native symbol and canonical signature.
pub struct Definition {
    /// Operation encoded by the guest PAL.
    pub operation: Operation,
    /// The wasi-libc symbol resolved by the native PAL.
    pub symbol: &'static str,
    /// The canonical name of the function the symbol lowers.
    pub name: &'static str,
    /// The scalar arguments the symbol takes; results return through guest memory.
    pub sig: Sig,
}
macro_rules! operations {
    ($( $variant:ident = $id:literal, $symbol:literal, $name:literal, [$($ty:ident),*]; )*) => {
        /// Operations on the read-only root; handles refer only to records in guest memory.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u32)]
        pub enum Operation {
            $(#[doc = $name] $variant = $id,)*
        }
        /// The single definition used by the engine adapter and native wrapper generator.
        pub const IMPORTS: &[Definition] = &[
            $(Definition {
                operation: Operation::$variant,
                symbol: $symbol,
                name: $name,
                sig: Sig { params: &[$(Ty::$ty),*], results: &[] },
            },)*
        ];
    }
}
operations! {
    Directories = 0, "filesystem_preopens_get_directories", "get-directories", [I32];
    DropDescriptor = 1, "filesystem_descriptor_drop", "[resource-drop]descriptor", [I32];
    DropDirectoryStream = 2, "filesystem_directory_entry_stream_drop", "[resource-drop]directory-entry-stream", [I32];
    ReadViaStream = 3, "filesystem_method_descriptor_read_via_stream", "[method]descriptor.read-via-stream", [I32, I64, I32];
    WriteViaStream = 4, "filesystem_method_descriptor_write_via_stream", "[method]descriptor.write-via-stream", [I32, I64, I32];
    AppendViaStream = 5, "filesystem_method_descriptor_append_via_stream", "[method]descriptor.append-via-stream", [I32, I32];
    Advise = 6, "filesystem_method_descriptor_advise", "[method]descriptor.advise", [I32, I64, I64, I32, I32];
    Flags = 7, "filesystem_method_descriptor_get_flags", "[method]descriptor.get-flags", [I32, I32];
    Type = 8, "filesystem_method_descriptor_get_type", "[method]descriptor.get-type", [I32, I32];
    Read = 9, "filesystem_method_descriptor_read", "[method]descriptor.read", [I32, I64, I64, I32];
    ReadDirectory = 10, "filesystem_method_descriptor_read_directory", "[method]descriptor.read-directory", [I32, I32];
    Stat = 11, "filesystem_method_descriptor_stat", "[method]descriptor.stat", [I32, I32];
    StatAt = 12, "filesystem_method_descriptor_stat_at", "[method]descriptor.stat-at", [I32, I32, I32, I32, I32];
    OpenAt = 13, "filesystem_method_descriptor_open_at", "[method]descriptor.open-at", [I32, I32, I32, I32, I32, I32, I32];
    ReadlinkAt = 14, "filesystem_method_descriptor_readlink_at", "[method]descriptor.readlink-at", [I32, I32, I32, I32];
    UnlinkFileAt = 15, "filesystem_method_descriptor_unlink_file_at", "[method]descriptor.unlink-file-at", [I32, I32, I32, I32];
    Hash = 16, "filesystem_method_descriptor_metadata_hash", "[method]descriptor.metadata-hash", [I32, I32];
    HashAt = 17, "filesystem_method_descriptor_metadata_hash_at", "[method]descriptor.metadata-hash-at", [I32, I32, I32, I32, I32];
    DirectoryEntry = 18, "filesystem_method_directory_entry_stream_read_directory_entry", "[method]directory-entry-stream.read-directory-entry", [I32, I32];
    SetSize = 19, "filesystem_method_descriptor_set_size", "[method]descriptor.set-size", [I32, I64, I32];
    Write = 20, "filesystem_method_descriptor_write", "[method]descriptor.write", [I32, I32, I32, I64, I32];
    Sync = 21, "filesystem_method_descriptor_sync", "[method]descriptor.sync", [I32, I32];
    CreateDirectoryAt = 22, "filesystem_method_descriptor_create_directory_at", "[method]descriptor.create-directory-at", [I32, I32, I32, I32];
    RemoveDirectoryAt = 23, "filesystem_method_descriptor_remove_directory_at", "[method]descriptor.remove-directory-at", [I32, I32, I32, I32];
    RenameAt = 24, "filesystem_method_descriptor_rename_at", "[method]descriptor.rename-at", [I32, I32, I32, I32, I32, I32, I32];
    SymlinkAt = 25, "filesystem_method_descriptor_symlink_at", "[method]descriptor.symlink-at", [I32, I32, I32, I32, I32, I32];
    SetTimes = 26, "filesystem_method_descriptor_set_times", "[method]descriptor.set-times", [I32, I32, I64, I32, I32, I64, I32, I32];
    LinkAt = 27, "filesystem_method_descriptor_link_at", "[method]descriptor.link-at", [I32, I32, I32, I32, I32, I32, I32, I32];
    SetTimesAt = 28, "filesystem_method_descriptor_set_times_at", "[method]descriptor.set-times-at", [I32, I32, I32, I32, I32, I64, I32, I32, I64, I32, I32];
}

/// The largest canonical scalar packet in the declared filesystem interface.
pub const MAX_ARGUMENTS: usize = {
    let mut maximum = 0;
    let mut index = 0;
    while index < IMPORTS.len() {
        if IMPORTS[index].sig.params.len() > maximum {
            maximum = IMPORTS[index].sig.params.len();
        }
        index += 1;
    }
    maximum
};

/// Generate the C wrappers which route wasi-libc calls through the Isoplane
/// adapter, and the guest allocator's filesystem record exports. They expect the
/// declarations of `isoplane_imports.h` and `isoplane_exports.h` before them.
pub fn native_wrappers() -> String {
    use std::fmt::Write;
    let mut output = String::from("/* Generated from isoplane_abi::filesystem::IMPORTS. */\n");
    output.push_str(&format!(
        "int32_t {}(int32_t size) {{ return (int32_t)(uintptr_t)malloc((uint32_t)size); }}\n",
        crate::exports::FS_ALLOC
    ));
    output.push_str(&format!(
        "void {}(int32_t pointer) {{ free((void *)(uintptr_t)(uint32_t)pointer); }}\n",
        crate::exports::FS_FREE
    ));
    for definition in IMPORTS {
        let signature = definition.sig;
        let parameters: Vec<String> = signature
            .params
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                format!(
                    "{} a{index}",
                    match ty {
                        Ty::I32 => "int32_t",
                        Ty::I64 => "int64_t",
                    }
                )
            })
            .collect();
        let arguments: Vec<String> = signature
            .params
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                format!(
                    "({})a{index}",
                    match ty {
                        Ty::I32 => "uint32_t",
                        Ty::I64 => "uint64_t",
                    }
                )
            })
            .collect();
        writeln!(output, "void __wasm_import_{}({}) {{ const uint64_t args[] = {{ {} }}; isoplane_filesystem_call({}, (int32_t)(uintptr_t)args, {}); }}", definition.symbol, parameters.join(", "), arguments.join(", "), definition.operation as u32, parameters.len()).expect("writing a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_served_files_lie_beneath_the_host_root() {
        for path in [super::APPLICATION_PACKAGE_ROOT, super::DEBUG_AGENT_PATH] {
            assert!(
                path.starts_with(&format!("{}/", super::HOST_ROOT)),
                "refusing application content beneath the host root must keep it from shadowing {path}"
            );
        }
    }
}
