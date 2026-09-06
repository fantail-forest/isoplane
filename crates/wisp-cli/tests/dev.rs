//! The actual CLI executes, rebuilds and reloads a project without a daemon.

#[path = "common/dev.rs"]
mod common;

use common::{guest, manifest, Dev};

#[tokio::test]
async fn a_csharp_project_publishes_and_reloads_offline_after_provisioning() {
    let dir = tempfile::tempdir().unwrap();
    let app = wisp_fixtures::dotnet_project(dir.path());
    let published = std::process::Command::new("dotnet")
        .args([
            "publish",
            "PublishGuest.csproj",
            "-c",
            "Release",
            "-o",
            "out",
            "--disable-build-servers",
        ])
        .current_dir(&app)
        .env("WISP_CLI", env!("CARGO_BIN_EXE_wisp"))
        .output()
        .expect("dotnet SDK must be installed to test C# publishing");
    assert!(
        published.status.success(),
        "direct C# publish must succeed: {}{}",
        String::from_utf8_lossy(&published.stdout),
        String::from_utf8_lossy(&published.stderr)
    );
    assert!(
        String::from_utf8_lossy(&published.stdout).contains("passes validator"),
        "direct publication must validate its real WASM output: {}",
        String::from_utf8_lossy(&published.stdout)
    );
    std::fs::write(app.join("wisp.toml"), "name='publish-test'\ntenant='test-owner'\nmodule='out/publish_guest.wasm'\n[dev]\ninit_sandbox='runtime-bringup'\nbuild='dotnet publish PublishGuest.csproj -c Release -o out --disable-build-servers -p:WispOffline=true'").unwrap();
    let mut dev = Dev::start(&app, &[]);
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "first build",
        "the initial offline development build must execute locally"
    );
    let source = app.join("Handler.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(&source, original.replace("first build", "offline rebuild")).unwrap();
    dev.wait_for("reloaded ");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "offline rebuild",
        "an offline C# edit must rebuild and execute replacement code"
    );
    std::fs::write(&source, "invalid C#").unwrap();
    dev.wait_for("reload failed; keeping the last working app");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "offline rebuild",
        "compiler failure must preserve the last working application"
    );
    std::fs::write(&source, original.replace("first build", "repaired build")).unwrap();
    dev.wait_for("reloaded ");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "repaired build",
        "a repaired C# source must resume development without restarting"
    );
    let project = app.join("PublishGuest.csproj");
    let xml = std::fs::read_to_string(&project).unwrap();
    std::fs::write(&project, xml.replace("</Project>", r#"
  <Target Name="HoldCompiler" BeforeTargets="WispCompilePal">
    <MakeDir Directories="$(IntermediateOutputPath)" />
    <Exec Command="mkfifo &quot;$(IntermediateOutputPath)cancel-gate&quot;; echo waiting-for-cancel; cat &quot;$(IntermediateOutputPath)cancel-gate&quot;" />
  </Target>
</Project>"#)).unwrap();
    let build_log = app.join(".wisp/build.log");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let log = std::fs::read_to_string(&build_log).unwrap();
        if log.contains("waiting-for-cancel") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "C# rebuild never reached its cancellation gate: {log}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    dev.stop();
    let log = std::fs::read_to_string(build_log).unwrap();
    assert!(
        log.contains("Attempting to cancel the build")
            || log.contains("C# publication cancelled")
            || log.contains("MSB5021"),
        "stopping local development must interrupt the active C# compiler: {log}"
    );
    if log.contains("compiling in local container") {
        assert!(
            log.contains("released compiler container"),
            "local development must await compiler container cleanup: {log}"
        );
    }
}

#[tokio::test]
async fn local_dev_builds_and_reloads_real_guest_code_and_keeps_a_good_build_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    manifest(dir.path(), "[dev]\nbuild='cp source.wasm app.wasm'");
    let source = dir.path().join("source.wasm");
    std::fs::write(&source, guest("first build")).unwrap();
    let mut dev = Dev::start(dir.path(), &[]);
    let client = reqwest::Client::new();
    let response = client.get(&dev.url).send().await.unwrap();
    assert_eq!(
        response.status(),
        200,
        "local guest must respond through HTTP"
    );
    assert_eq!(
        response.text().await.unwrap(),
        "first build",
        "the initial project build must execute on the first request"
    );

    std::fs::write(&source, guest("second build")).unwrap();
    dev.wait_for("reloaded ");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "second build",
        "source changes must execute the replacement module"
    );

    std::fs::write(&source, b"invalid wasm").unwrap();
    dev.wait_for("reload failed; keeping the last working app");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "second build",
        "rejected replacement code must leave the working app available"
    );

    std::fs::write(&source, guest("repaired build")).unwrap();
    dev.wait_for("reloaded ");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "repaired build",
        "a valid replacement must become available after a rejected reload"
    );

    std::fs::remove_file(&source).unwrap();
    dev.wait_for("reload failed; keeping the last working app");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "repaired build",
        "a failing build command must also leave the working app available"
    );
    dev.stop();
}

#[tokio::test]
async fn local_dev_preinitializes_and_executes_a_csharp_guest() {
    let bytes = wisp_fixtures::dotnet("wisp_guest_dotnet");
    let dir = tempfile::tempdir().unwrap();
    manifest(dir.path(), "[dev]\ninit_sandbox='runtime-bringup'");
    std::fs::write(dir.path().join("app.wasm"), bytes).unwrap();
    let mut dev = Dev::start(dir.path(), &["--no-build", "--no-watch"]);
    let response = reqwest::get(&dev.url).await.unwrap();
    assert_eq!(
        response.status(),
        404,
        "the C# handler returns a cache miss when local storage is empty"
    );
    let body = response.text().await.unwrap();
    assert_eq!(
        body, "miss",
        "the C# await must resume with the local storage result"
    );
    dev.stop();
}

#[test]
fn a_second_local_process_cannot_share_the_projects_state() {
    let dir = tempfile::tempdir().unwrap();
    manifest(dir.path(), "");
    std::fs::write(dir.path().join("app.wasm"), guest("owner")).unwrap();
    let mut dev = Dev::start(dir.path(), &["--no-build"]);
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_wisp"))
        .args(["dev", "--no-build", "--watch"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(
        !result.status.success(),
        "a second development process must not open the live project's state"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("another wisp dev"),
        "the failure must explain who owns the project"
    );
    dev.stop();
}

#[test]
fn the_cli_does_not_depend_on_node_administration_or_the_deployment_control_plane() {
    let result = std::process::Command::new("cargo")
        .args([
            "tree",
            "--offline",
            "-p",
            "wisp-cli",
            "--edges",
            "normal",
            "--prefix",
            "none",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "the CLI dependency graph must be inspectable: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let tree = String::from_utf8(result.stdout).unwrap();
    assert!(
        !tree.lines().any(|line| matches!(
            line.split_whitespace().next(),
            Some("wispd" | "wisp-control")
        )),
        "shipping local development must not pull the hosted control plane into the CLI:\n{tree}"
    );
}
