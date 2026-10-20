//! `isoplaned --check`: config validation from the binary's exit code and
//! stderr, independent of a running node.

#[test]
fn the_binary_accepts_a_partitioned_config() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("good.toml");
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
limits.instances_per_application = 3
limits.module_bytes_compressed = 5242880
limits.package_bytes = 52428800
limits.instance_memory_max = 134217728
limits.occupancy_ms_per_dispatch = 100
limits.subrequests_per_dispatch = 20
limits.pending_futures_per_dispatch = 6
cell = "cell-0"
versions = [{ id = "v1" }]

[[apps]]
name = "paying"
tenant = "bigco"
limits.instances_per_application = 8
limits.module_bytes_compressed = 52428800
limits.package_bytes = 52428800
limits.instance_memory_max = 134217728
limits.occupancy_ms_per_dispatch = 30000
limits.subrequests_per_dispatch = 200
limits.pending_futures_per_dispatch = 64
cell = "cell-1"
versions = [{ id = "v1" }]
"#,
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"))
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
fn the_binary_refuses_an_operator_trust_bundle_whose_hash_does_not_match() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let bundle = dir.path().join("roots.pem");
    std::fs::write(
        &bundle,
        "-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let path = dir.path().join("bad-trust.toml");
    let config = toml::toml! {
        node = "n1"
        [[cells]]
        name = "cell-1"
        [trust]
        bundle = (bundle.to_str().expect("temporary path is UTF-8"))
        sha256 = ("0".repeat(64))
    };
    std::fs::write(
        &path,
        toml::to_string(&config).expect("configuration serializes"),
    )
    .unwrap();

    let out = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"))
        .args(["--config", path.to_str().unwrap(), "--check"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a bundle that is not the one recorded must refuse, not warn. stderr: {stderr}"
    );
    assert!(
        stderr.contains("trust.sha256") && stderr.contains("roots.pem"),
        "the refusal must name the bundle and where the expected hash came from: {stderr}"
    );
}

#[test]
fn the_binary_reports_the_shipped_trust_bundle_when_it_accepts_a_config() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let path = dir.path().join("good.toml");
    std::fs::write(
        &path,
        r#"
node = "n1"

[[cells]]
name = "cell-1"
"#,
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"))
        .args(["--config", path.to_str().unwrap(), "--check"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "a config without a [trust] section serves the shipped bundle: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../../trust/ca-bundle.manifest.json"))
            .expect("the shipped manifest is JSON");
    let expected = format!(
        "trust bundle {} generation {} sha256 {}",
        manifest["bundle_date"].as_str().unwrap(),
        manifest["generation"].as_u64().unwrap(),
        &manifest["bundle_sha256"].as_str().unwrap()[..12]
    );
    assert!(
        stdout.contains(&expected),
        "the check output must state which bundle a node on this config would serve; \
         expected `{expected}` in: {stdout}"
    );
}

#[test]
fn the_binary_refuses_a_hash_valid_bundle_with_malformed_certificates() {
    use sha2::Digest;
    let dir = tempfile::tempdir().expect("a temporary directory");
    let bundle = dir.path().join("roots.pem");
    let pem = b"-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n";
    std::fs::write(&bundle, pem).expect("write malformed certificate bytes");
    let path = dir.path().join("bad-trust.toml");
    let config = toml::toml! {
        node = "trust-test"
        [[cells]]
        name = "cell-1"
        [trust]
        bundle = (bundle.to_str().expect("temporary path is UTF-8"))
        sha256 = (hex::encode(sha2::Sha256::digest(pem)))
    };
    std::fs::write(
        &path,
        toml::to_string(&config).expect("configuration serializes"),
    )
    .expect("write node configuration");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"))
        .arg("--config")
        .arg(&path)
        .arg("--check")
        .output()
        .expect("the daemon's checker starts");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "matching the hash must not let an unusable trust bundle start serving: {stderr}"
    );
    assert!(
        stderr.contains("invalid certificate") && stderr.contains("roots.pem"),
        "the diagnostic must identify the malformed bundle and certificate: {stderr}"
    );
}
