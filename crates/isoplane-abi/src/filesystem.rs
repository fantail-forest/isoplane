//! Read-only filesystem adapter operations and their canonical WASI signatures.
//! The root is empty but for a host-declared set of read-only regular files,
//! readable only through `descriptor.read`.
use crate::imports::{Sig, Ty};
use crate::pal::{Delegation, Disposition, PalDef, FS_PREOPENS, FS_TYPES};

/// The only Isoplane entry used by the native filesystem PAL wrappers.
pub const IMPORT: &str = "filesystem_call";

/// An operation with its native symbol and admitted canonical signature.
pub struct Definition {
    /// Operation encoded by the guest PAL.
    pub operation: Operation,
    /// The wasi-libc symbol resolved by the native PAL.
    pub symbol: &'static str,
    /// The corresponding raw WASI import.
    pub pal: PalDef,
}
macro_rules! operations {
    ($( $variant:ident = $id:literal, $symbol:literal, $module:ident, $name:literal, [$($ty:ident),*]; )*) => {
        /// Operations on the read-only root; handles refer only to records in guest memory.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(u32)]
        pub enum Operation {
            $(#[doc = $name] $variant = $id,)*
        }
        /// The single definition used by the validator, adapters and native wrapper generator.
        pub const IMPORTS: &[Definition] = &[
            $(Definition {
                operation: Operation::$variant,
                symbol: $symbol,
                pal: PalDef {
                    module: $module,
                    func: $name,
                    disposition: Disposition::Delegate(Delegation::Cap(IMPORT)),
                    sig: Some(Sig { params: &[$(Ty::$ty),*], results: &[] }),
                    why: "read-only root holding only host-declared platform files, readable through descriptor.read alone; undeclared paths return no-entry, mutations and write-enabling opens return read-only, streams return not-permitted, invalid handles return bad-descriptor; no host filesystem access",
                },
            },)*
        ];
    }
}
operations! {
    Directories = 0, "filesystem_preopens_get_directories", FS_PREOPENS, "get-directories", [I32];
    DropDescriptor = 1, "filesystem_descriptor_drop", FS_TYPES, "[resource-drop]descriptor", [I32];
    DropDirectoryStream = 2, "filesystem_directory_entry_stream_drop", FS_TYPES, "[resource-drop]directory-entry-stream", [I32];
    ReadViaStream = 3, "filesystem_method_descriptor_read_via_stream", FS_TYPES, "[method]descriptor.read-via-stream", [I32, I64, I32];
    WriteViaStream = 4, "filesystem_method_descriptor_write_via_stream", FS_TYPES, "[method]descriptor.write-via-stream", [I32, I64, I32];
    AppendViaStream = 5, "filesystem_method_descriptor_append_via_stream", FS_TYPES, "[method]descriptor.append-via-stream", [I32, I32];
    Advise = 6, "filesystem_method_descriptor_advise", FS_TYPES, "[method]descriptor.advise", [I32, I64, I64, I32, I32];
    Flags = 7, "filesystem_method_descriptor_get_flags", FS_TYPES, "[method]descriptor.get-flags", [I32, I32];
    Type = 8, "filesystem_method_descriptor_get_type", FS_TYPES, "[method]descriptor.get-type", [I32, I32];
    Read = 9, "filesystem_method_descriptor_read", FS_TYPES, "[method]descriptor.read", [I32, I64, I64, I32];
    ReadDirectory = 10, "filesystem_method_descriptor_read_directory", FS_TYPES, "[method]descriptor.read-directory", [I32, I32];
    Stat = 11, "filesystem_method_descriptor_stat", FS_TYPES, "[method]descriptor.stat", [I32, I32];
    StatAt = 12, "filesystem_method_descriptor_stat_at", FS_TYPES, "[method]descriptor.stat-at", [I32, I32, I32, I32, I32];
    OpenAt = 13, "filesystem_method_descriptor_open_at", FS_TYPES, "[method]descriptor.open-at", [I32, I32, I32, I32, I32, I32, I32];
    ReadlinkAt = 14, "filesystem_method_descriptor_readlink_at", FS_TYPES, "[method]descriptor.readlink-at", [I32, I32, I32, I32];
    UnlinkFileAt = 15, "filesystem_method_descriptor_unlink_file_at", FS_TYPES, "[method]descriptor.unlink-file-at", [I32, I32, I32, I32];
    Hash = 16, "filesystem_method_descriptor_metadata_hash", FS_TYPES, "[method]descriptor.metadata-hash", [I32, I32];
    HashAt = 17, "filesystem_method_descriptor_metadata_hash_at", FS_TYPES, "[method]descriptor.metadata-hash-at", [I32, I32, I32, I32, I32];
    DirectoryEntry = 18, "filesystem_method_directory_entry_stream_read_directory_entry", FS_TYPES, "[method]directory-entry-stream.read-directory-entry", [I32, I32];
    SetSize = 19, "filesystem_method_descriptor_set_size", FS_TYPES, "[method]descriptor.set-size", [I32, I64, I32];
    Write = 20, "filesystem_method_descriptor_write", FS_TYPES, "[method]descriptor.write", [I32, I32, I32, I64, I32];
    Sync = 21, "filesystem_method_descriptor_sync", FS_TYPES, "[method]descriptor.sync", [I32, I32];
    CreateDirectoryAt = 22, "filesystem_method_descriptor_create_directory_at", FS_TYPES, "[method]descriptor.create-directory-at", [I32, I32, I32, I32];
    RemoveDirectoryAt = 23, "filesystem_method_descriptor_remove_directory_at", FS_TYPES, "[method]descriptor.remove-directory-at", [I32, I32, I32, I32];
    RenameAt = 24, "filesystem_method_descriptor_rename_at", FS_TYPES, "[method]descriptor.rename-at", [I32, I32, I32, I32, I32, I32, I32];
    SymlinkAt = 25, "filesystem_method_descriptor_symlink_at", FS_TYPES, "[method]descriptor.symlink-at", [I32, I32, I32, I32, I32, I32];
    SetTimes = 26, "filesystem_method_descriptor_set_times", FS_TYPES, "[method]descriptor.set-times", [I32, I32, I64, I32, I32, I64, I32, I32];
    LinkAt = 27, "filesystem_method_descriptor_link_at", FS_TYPES, "[method]descriptor.link-at", [I32, I32, I32, I32, I32, I32, I32, I32];
    SetTimesAt = 28, "filesystem_method_descriptor_set_times_at", FS_TYPES, "[method]descriptor.set-times-at", [I32, I32, I32, I32, I32, I64, I32, I32, I64, I32, I32];
}

/// The largest canonical scalar packet in the declared filesystem interface.
pub const MAX_ARGUMENTS: usize = {
    let mut maximum = 0;
    let mut index = 0;
    while index < IMPORTS.len() {
        if let Some(signature) = IMPORTS[index].pal.sig {
            if signature.params.len() > maximum {
                maximum = signature.params.len();
            }
        }
        index += 1;
    }
    maximum
};

/// Generate the C wrappers which route wasi-libc calls through the Isoplane adapter.
pub fn native_wrappers() -> String {
    use std::fmt::Write;
    let mut output = String::from("/* Generated from isoplane_abi::filesystem::IMPORTS. */\n");
    output.push_str("__attribute__((import_module(\"isoplane\"), import_name(\"filesystem_call\")))\nextern void isoplane_filesystem_call(int32_t operation, const uint64_t *arguments, int32_t count);\n");
    output.push_str(&format!("__attribute__((export_name(\"{}\")))\nuint32_t isoplane_fs_alloc(uint32_t size) {{ return (uint32_t)(uintptr_t)malloc(size); }}\n", crate::exports::FS_ALLOC));
    output.push_str(&format!("__attribute__((export_name(\"{}\")))\nvoid isoplane_fs_free(uint32_t pointer) {{ free((void *)(uintptr_t)pointer); }}\n", crate::exports::FS_FREE));
    for definition in IMPORTS {
        let signature = definition
            .pal
            .sig
            .expect("every filesystem definition in this function's IMPORTS has a signature");
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
        writeln!(output, "void __wasm_import_{}({}) {{ const uint64_t args[] = {{ {} }}; isoplane_filesystem_call({}, args, {}); }}", definition.symbol, parameters.join(", "), arguments.join(", "), definition.operation as u32, parameters.len()).expect("writing a String cannot fail");
    }
    output
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_filesystem_wrappers_match_the_single_abi_definition() {
        assert_eq!(super::native_wrappers(), include_str!("../../../guest-sdks/dotnet/native/isoplane_filesystem.h"), "regenerate the native filesystem wrappers with cargo run -p isoplane-abi --example filesystem_header");
    }
}
