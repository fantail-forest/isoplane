use isoplane_cache::{Cache, CacheBytes, CacheKey};
use std::{fs, path::Path};

#[test]
fn the_cleanup_script_obeys_rust_leases_and_reclaims_released_entries() {
    let root = tempfile::tempdir().unwrap();
    let cache = Cache::new(root.path().into(), CacheBytes(1024)).unwrap();
    let lease = cache.lease(CacheKey([3; 32])).unwrap();
    let path = lease.directory().to_path_buf();
    fs::write(path.join("module"), b"compiled").unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/build_storage.py");
    let clean = || {
        let status = std::process::Command::new("python3")
            .arg("-c")
            .arg("import importlib.util,sys; s=importlib.util.spec_from_file_location('storage',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); m.prune(sys.argv[2],0)")
            .arg(&script)
            .arg(root.path())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "the cleanup script must understand the Rust cache layout"
        );
    };
    clean();
    assert!(
        path.exists(),
        "the cleanup script must preserve a Rust process's active mappings"
    );
    drop(lease);
    clean();
    assert!(
        !path.exists(),
        "the cleanup script must reclaim a released Rust cache entry"
    );
}
