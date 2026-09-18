//! Installed Isoplane development and debugging journeys.

#[path = "common/dev.rs"]
pub mod common;
use common::Dev;

#[tokio::test]
async fn an_installed_dotnet_tool_runs_and_reloads_the_aspnet_template() {
    let dir = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(dir.path());
    let manifest_path = app.join("isoplane.toml");
    let manifest_text = std::fs::read_to_string(&manifest_path).unwrap();
    let mut manifest: toml::Value = toml::from_str(&manifest_text).unwrap();
    manifest["module"] = toml::Value::String("dist/nested/customer.wasm".into());
    std::fs::write(&manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
    let cli = isoplane_fixtures::managed_cli(dir.path(), &app);
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
    let app = isoplane_fixtures::aspnet_project(directory.path());
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
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
    let launcher = std::env::var_os("ISOPLANE_DEBUG_ADAPTER_LAUNCHER")
        .expect("set ISOPLANE_DEBUG_ADAPTER_LAUNCHER to the external Mono DAP adapter launcher");
    let python = std::env::var_os("ISOPLANE_DAP_PYTHON").expect(
        "set ISOPLANE_DAP_PYTHON to a Python interpreter with debugpy installed for DAP framing",
    );
    let directory = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(directory.path());
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
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
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
        !app.join(".isoplane/state").exists(),
        "a disposable debugger session must not leave runtime state in the application's persistent directory"
    );
    assert!(
        std::fs::read_dir(app.join(".isoplane/debug"))
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
    let app = isoplane_fixtures::aspnet_project(directory.path());
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
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
            !app.join(".isoplane/debug").exists(),
            "invalid debug options must not create a session"
        );
    }
}
