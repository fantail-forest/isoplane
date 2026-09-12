//! The actual CLI executes, rebuilds and reloads a project without a daemon.

#[path = "common/dev.rs"]
mod common;

use common::{guest, manifest, Dev};

#[test]
fn dotnet_clean_removes_the_published_guest() {
    let dir = tempfile::tempdir().unwrap();
    let app = wisp_fixtures::dotnet_project(dir.path());
    for command in ["publish", "clean"] {
        let mut process = std::process::Command::new("dotnet");
        process.args([command, "PublishGuest.csproj", "-c", "Release"]);
        if command == "publish" {
            process.args(["-o", "out", "--disable-build-servers"]);
        }
        let result = process
            .current_dir(&app)
            .env("WISP_CLI", env!("CARGO_BIN_EXE_wisp"))
            .env(
                "WISP_GUEST_RUNTIME_FEED",
                wisp_fixtures::guest_runtime_feed(),
            )
            .output()
            .expect("dotnet SDK must be installed to test publication cleanup");
        assert!(
            result.status.success(),
            "dotnet {command} must succeed: {}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            app.join("out/publish_guest.wasm").exists(),
            command == "publish",
            "publication must create the guest and clean must remove it"
        );
        let properties = std::process::Command::new("dotnet")
            .args([
                "msbuild",
                "PublishGuest.csproj",
                "-p:Configuration=Release",
                "-getProperty:IntermediateOutputPath",
            ])
            .current_dir(&app)
            .output()
            .expect("query the publication fixture's intermediate directory");
        assert!(
            properties.status.success(),
            "the publication fixture must expose its actual intermediate directory: {}",
            String::from_utf8_lossy(&properties.stderr)
        );
        let intermediate = app.join(String::from_utf8(properties.stdout).unwrap().trim());
        let clock = intermediate.join("wisp_clock.o");
        assert_eq!(
            clock.is_file(),
            command == "publish",
            "publication must compile the shared clock object and clean must remove it"
        );
    }
}

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
        .env(
            "WISP_GUEST_RUNTIME_FEED",
            wisp_fixtures::guest_runtime_feed(),
        )
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
    dev.wait_event("reloaded");
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
    dev.wait_event("reloaded");
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
    manifest(
        dir.path(),
        "[dev]\nbuild='echo build-output; cp source.wasm app.wasm'",
    );
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
    dev.wait_event("reloaded");
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
    dev.wait_event("reloaded");
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

#[tokio::test]
async fn an_installed_dotnet_tool_runs_and_reloads_the_aspnet_template() {
    let dir = tempfile::tempdir().unwrap();
    let app = wisp_fixtures::aspnet_project(dir.path());
    let manifest_path = app.join("wisp.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path).unwrap();
    let mut manifest: toml::Value = toml::from_str(&manifest_text).unwrap();
    manifest["module"] = toml::Value::String("dist/nested/customer.wasm".into());
    std::fs::write(&manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
    let cli = wisp_fixtures::managed_cli(
        dir.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_wisp")),
        &app,
    );
    let mut dev = Dev::start_managed(&app, &cli);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();
    let response = client.get(&dev.url).send().await.unwrap();
    assert_eq!(
        response.status(),
        200,
        "the installed ASP.NET template must serve successfully through the local sandbox"
    );
    assert_eq!(
        response.text().await.unwrap(),
        "Hello from Isoplane!",
        "the generated ASP.NET route must execute without copied interop boilerplate"
    );
    let source = app.join("Application.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(
        &source,
        original.replace("Hello from Isoplane!", "Rebuilt by the .NET CLI"),
    )
    .unwrap();
    dev.wait_event("reloaded");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "Rebuilt by the .NET CLI",
        "editing source must publish and reload through the installed .NET tool"
    );
    std::fs::write(&source, "invalid C#").unwrap();
    dev.wait_for("rebuild failed; keeping the last working application");
    assert_eq!(
        client
            .get(&dev.url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "Rebuilt by the .NET CLI",
        "failed managed builds must retain the last working guest"
    );
    dev.stop();
}

#[tokio::test]
async fn an_installed_mono_profile_serves_the_template_and_resumes_ordinary_timers() {
    let directory = tempfile::tempdir().unwrap();
    let app = wisp_fixtures::aspnet_project(directory.path());
    let cli = wisp_fixtures::managed_cli(
        directory.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_wisp")),
        &app,
    );
    let mut dev = Dev::start_managed_with_args(&app, &cli, &["--guest-runtime", "mono"]);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();
    let response = client.get(&dev.url).send().await.unwrap();
    assert_eq!(
        response.status(),
        200,
        "the explicit Mono profile must serve the unchanged installed template"
    );
    assert_eq!(
        response.text().await.unwrap(),
        "Hello from Isoplane!",
        "Mono must invoke the generated ASP.NET exports through the actual sandbox"
    );
    let source = app.join("Application.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(&source, original.replace("return app;", "app.MapGet(\"/async\", async () => { await System.Threading.Tasks.Task.Delay(1); return \"async-ready\"; }); return app;")).unwrap();
    dev.wait_event("reloaded");
    let response = client
        .get(format!("{}/async", dev.url))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "the async route containing ordinary Task.Delay must complete successfully"
    );
    assert_eq!(
        response.text().await.unwrap(),
        "async-ready",
        "the async Mono route must return its expected response body"
    );
    dev.stop();
}

fn installed_debug_acceptance(mode: &str) {
    let launcher = std::env::var_os("WISP_DEBUG_ADAPTER_LAUNCHER")
        .expect("set WISP_DEBUG_ADAPTER_LAUNCHER to the external Mono DAP adapter launcher");
    let python = std::env::var_os("WISP_DAP_PYTHON").expect(
        "set WISP_DAP_PYTHON to a Python interpreter with debugpy installed for DAP framing",
    );
    let directory = tempfile::tempdir().unwrap();
    let app = wisp_fixtures::aspnet_project(directory.path());
    let source = app.join("Application.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    let modified = original.replace(
        "app.MapGet(\"/\", () => \"Hello from Isoplane!\");",
        "app.MapGet(\"/\", Handle);",
    );
    let position = modified.rfind('}').unwrap();
    let handler = r#"
    private static async Task<string> Handle()
    {
        int answer = 42;
        Task delay = Task.Delay(100);
        if (delay.IsCompleted) throw new InvalidOperationException("host timer must be incomplete before awaiting");
        await delay;
        bool resumed = true;
        return resumed ? answer.ToString() : throw new InvalidOperationException("continuation did not run");
    }
"#;
    std::fs::write(
        &source,
        format!(
            "{}{}{}",
            &modified[..position],
            handler,
            &modified[position..]
        ),
    )
    .unwrap();
    let cli = wisp_fixtures::managed_cli(
        directory.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_wisp")),
        &app,
    );
    let mut dev = Dev::start_managed_with_args(&app, &cli, &["--debug", "--no-watch"]);
    let metadata = dev
        .debugger
        .as_ref()
        .expect("debug CLI must report its exact relay and module/symbol pair");
    let driver =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/common/debug_driver.py");
    let result = std::process::Command::new(python)
        .arg(driver)
        .arg(launcher)
        .arg(metadata["endpoint"].as_str().unwrap())
        .arg(&dev.url)
        .arg(source)
        .arg(dev.pid().to_string())
        .arg(mode)
        .arg(metadata["module"].as_str().unwrap())
        .output()
        .expect("run the real external Mono DAP adapter acceptance driver");
    assert!(
        result.status.success(),
        "installed debug {mode} acceptance failed: {}{}\nRuntime output:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        dev.diagnostics()
    );
    dev.wait_exit();
    assert!(
        !app.join(".wisp/state").exists(),
        "a disposable debugger session must not leave runtime state in the application's persistent directory"
    );
    assert!(
        std::fs::read_dir(app.join(".wisp/debug"))
            .unwrap()
            .next()
            .is_none(),
        "debugger exit must remove the entire owned session, including runtime scratch"
    );
}

// TODO(#105): Restore this test when the modern .NET adapter and its test prerequisites are packaged.
#[test]
#[ignore = "requires the modern .NET debugger adapter and packaged test prerequisites (#105)"]
fn installed_debugging_inspects_a_local_and_steps_across_a_host_await() {
    installed_debug_acceptance("continue");
}

// TODO(#105): Restore this test when the modern .NET adapter and its test prerequisites are packaged.
#[test]
#[ignore = "requires the modern .NET debugger adapter and packaged test prerequisites (#105)"]
fn stopping_the_installed_cli_terminates_a_paused_debug_session() {
    installed_debug_acceptance("stop");
}

#[test]
fn installed_debug_options_reject_watch_and_nativeaot_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let app = wisp_fixtures::aspnet_project(directory.path());
    let cli = wisp_fixtures::managed_cli(
        directory.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_wisp")),
        &app,
    );
    for (arguments, diagnostic) in [
        (vec!["--debug"], "--debug requires --no-watch"),
        (
            vec!["--debug", "--no-watch", "--guest-runtime", "nativeaot"],
            "--debug requires the Mono guest runtime",
        ),
    ] {
        let result = std::process::Command::new(&cli)
            .args(["dev", "--project"])
            .arg(&app)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "unsupported debug modes must fail before publication"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(diagnostic),
            "debug mode rejection must explain the supported workflow: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            !app.join(".wisp/debug").exists(),
            "invalid debug options must not create a session"
        );
    }
}
