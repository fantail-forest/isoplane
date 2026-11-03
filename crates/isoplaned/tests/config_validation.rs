//! Configuration validation through the daemon's command-line interface.

use std::path::Path;
use std::process::Output;

fn check(path: &Path) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"))
        .arg("serve")
        .arg("--config")
        .arg(path)
        .arg("--check")
        .output()
        .expect("the daemon starts")
}



#[test]
fn the_binary_accepts_several_configured_applications() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("good.toml");
    std::fs::write(
        &path,
        r#"
owner = "acme"
[preparation]
runtimes = "installed-runtimes"
[[apps]]
hosts = ["quotes.example"]
manifest = { name = "quotes", guest_memory_bytes = 16777216 }
[[apps]]
hosts = ["orders.example"]
manifest = { name = "orders", guest_memory_bytes = 16777216 }
"#,
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"))
        .arg("serve")
        .arg("--config")
        .arg(&path)
        .arg("--print-config")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let config: isoplaned::NodeConfig =
        toml::from_str(std::str::from_utf8(&out.stdout).unwrap()).unwrap();
    assert_eq!(config.apps.len(), 2);
    assert_eq!(config.owner, "acme");
}

#[test]
fn invalid_routing_fails_before_creating_the_data_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.toml");
    std::fs::write(
        &path,
        r#"
owner = "acme"
data_dir = "state"
[[apps]]
hosts = ["api.example"]
manifest = { name = "quotes", guest_memory_bytes = 16777216 }
[[apps]]
hosts = ["API.example"]
manifest = { name = "orders", guest_memory_bytes = 16777216 }
"#,
    )
    .unwrap();
    let out = check(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "duplicate routes were accepted");
    assert!(
        stderr.contains("routing"),
        "missing routing diagnostic: {stderr}"
    );
    assert!(
        !dir.path().join("state").exists(),
        "invalid configuration created state"
    );
}

#[test]
fn the_binary_refuses_an_operator_trust_bundle_whose_hash_does_not_match() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("roots.pem");
    std::fs::write(
        &bundle,
        "-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    let path = dir.path().join("bad-trust.toml");
    let config = toml::toml! {
        owner = "operator"
        [trust]
        bundle = "roots.pem"
        sha256 = ("0".repeat(64))
    };
    std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let out = check(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a mismatched bundle was accepted");
    assert!(
        stderr.contains("trust.sha256") && stderr.contains("roots.pem"),
        "missing bundle diagnostic: {stderr}"
    );
}

#[test]
fn the_binary_refuses_a_hash_valid_bundle_with_malformed_certificates() {
    use sha2::Digest;
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("roots.pem");
    let pem = b"-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n";
    std::fs::write(&bundle, pem).unwrap();
    let path = dir.path().join("bad-trust.toml");
    let config = toml::toml! {
        owner = "operator"
        [trust]
        bundle = "roots.pem"
        sha256 = (hex::encode(sha2::Sha256::digest(pem)))
    };
    std::fs::write(&path, toml::to_string(&config).unwrap()).unwrap();
    let out = check(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "an invalid certificate was accepted");
    assert!(
        stderr.contains("invalid certificate") && stderr.contains("roots.pem"),
        "missing certificate diagnostic: {stderr}"
    );
}

fn missing_runtime_configuration_is_refused(preparation: &str) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.toml");
    std::fs::write(
        &path,
        format!(
            "owner = 'operator'\ndata_dir = 'state'\n{preparation}\n[[apps]]\nhosts = ['app.example']\nmanifest = {{ name = 'app' }}\n"
        ),
    )
    .unwrap();
    for check_only in [true, false] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_isoplaned"));
        command.args(["serve", "--config"]).arg(&path);
        if check_only {
            command.arg("--check");
        }
        let output = command.output().unwrap();
        assert!(
            !output.status.success(),
            "an application without an installed runtime location passed validation"
        );
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("preparation.runtimes"), "{diagnostic}");
        assert!(
            !directory.path().join("state").exists(),
            "invalid runtime configuration created host state"
        );
    }
}

#[test]
fn configured_applications_require_preparation_before_creating_state() {
    missing_runtime_configuration_is_refused("");
}

#[test]
fn configured_applications_require_a_runtime_location_before_creating_state() {
    missing_runtime_configuration_is_refused("[preparation]");
}

#[test]
fn static_validation_needs_a_runtime_location_but_does_not_load_its_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.toml");
    std::fs::write(
        &path,
        "owner = 'operator'\ndata_dir = 'state'\n[preparation]\nruntimes = 'not-installed'\n[[apps]]\nhosts = ['app.example']\nmanifest = { name = 'app' }\n",
    )
    .unwrap();
    let output = check(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!directory.path().join("state").exists());
}

#[test]
fn an_empty_host_requires_no_runtime_installation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("host.toml");
    std::fs::write(&path, "owner = 'operator'\ndata_dir = 'state'\n").unwrap();
    let output = check(&path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!directory.path().join("state").exists());
}
