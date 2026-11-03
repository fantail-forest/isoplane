//! Minimal installed runtime for tests that register a directly prepared WAT guest.

use sha2::Digest;
use std::path::{Path, PathBuf};

pub fn install(directory: &Path) -> PathBuf {
    let inventory = directory.join("test-runtime-inventory");
    let release = inventory.join("1.0.0");
    std::fs::create_dir_all(&release).unwrap();
    let module = b"\0asm\x01\0\0\0";
    let module_name = isoplane_prepare::package::RUNTIME_MODULE;
    std::fs::write(release.join(module_name), module).unwrap();
    std::fs::write(
        release.join("runtime.json"),
        serde_json::to_vec(&serde_json::json!({
            "sdkVersion": "1.0.0",
            "sourceRevision": "fixture",
            "modules": { module_name: { "sha256": hex::encode(sha2::Sha256::digest(module)) } }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        inventory.join("inventory.json"),
        br#"{"sdkVersions":["1.0.0"]}"#,
    )
    .unwrap();
    inventory
}
