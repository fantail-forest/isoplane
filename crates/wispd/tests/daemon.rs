//! Daemon command configuration.

#[test]
fn the_binary_accepts_a_partitioned_config() {
    let dir = std::env::temp_dir().join(format!("wispd-test-ok-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("good.toml");
    std::fs::write(
        &path,
        r#"
node = "n1"

[[cells]]
name = "cell-0"

[[cells]]
name = "cell-1"

[[apps]]
name = "freebie"
tenant = "student"
limits.module_bytes_compressed = 5242880
limits.instance_memory_max = 67108864
limits.cpu_ms_per_http_dispatch = 10
limits.subrequests_per_dispatch = 20
limits.pending_futures_per_dispatch = 6
cell = "cell-0"
versions = [{ id = "v1" }]

[[apps]]
name = "paying"
tenant = "bigco"
limits.module_bytes_compressed = 20971520
limits.instance_memory_max = 134217728
limits.cpu_ms_per_http_dispatch = 50
limits.subrequests_per_dispatch = 200
limits.pending_futures_per_dispatch = 64
cell = "cell-1"
versions = [{ id = "v1" }]
"#,
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_wispd"))
        .args(["--config", path.to_str().unwrap(), "--check"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
