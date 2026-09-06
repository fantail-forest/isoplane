//! `wispd --check`: config validation from the binary's exit code and
//! stderr, independent of a running node.

#[test]
fn the_binary_accepts_a_partitioned_config() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("good.toml");
    std::fs::write(
        &path,
        r#"
deployment = "development"
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
limits.cpu_ms_per_http_dispatch = 30000
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
        "a partitioned config is accepted: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_todo_sample_configuration_is_accepted_by_the_daemon() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../guests/todo-cs/wispd.toml");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_wispd"))
        .arg("--config")
        .arg(path)
        .arg("--check")
        .output()
        .expect("the daemon's configuration checker can be started");
    assert!(
        output.status.success(),
        "the shipped todo sample must be runnable with the current configuration schema: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
