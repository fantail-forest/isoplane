//! Local project discovery through the installed runtime command.

mod common;

use common::run;

#[test]
fn project_resolves_a_custom_module_without_building_it() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("wisp.toml"),
        "name='custom'\ntenant='owner'\nmodule='dist/nested/customer.wasm'\n[dev]\nbuild='exit 73'",
    )
    .unwrap();
    let output = run(&["project", "--directory", directory.path().to_str().unwrap()]);
    assert_eq!(
        output.code,
        Some(0),
        "project discovery must succeed before publication: {}",
        output.stderr
    );
    let module = directory
        .path()
        .canonicalize()
        .unwrap()
        .join("dist/nested/customer.wasm");
    let actual: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
    assert_eq!(
        actual,
        serde_json::json!({ "module": module }),
        "the command must resolve the configured custom path as JSON"
    );
    assert!(
        !module.exists(),
        "project discovery must not build the configured module"
    );
    assert!(
        !directory.path().join(".wisp").exists(),
        "project discovery must not start a local runtime session"
    );
}

#[test]
fn project_refuses_a_manifest_without_tenant_identity() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("wisp.toml"),
        "name='custom'\nmodule='out/app.wasm'",
    )
    .unwrap();
    let output = run(&["project", "--directory", directory.path().to_str().unwrap()]);
    assert_eq!(
        output.code,
        Some(1),
        "an invalid local manifest must fail discovery"
    );
    assert!(
        output.stdout.is_empty(),
        "invalid project inputs must not produce a usable discovery result"
    );
    assert!(
        output.stderr.contains("tenant"),
        "the error must identify the missing tenant: {}",
        output.stderr
    );
}
