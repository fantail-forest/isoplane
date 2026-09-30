//! Installed Isoplane development and debugging journeys.

#[path = "common/dev.rs"]
pub mod common;
#[path = "common/debugger.rs"]
mod debugger;
use common::Dev;

#[tokio::test]
async fn an_installed_dotnet_tool_runs_and_reloads_the_aspnet_template() {
    let dir = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(dir.path());
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
async fn an_installed_application_resumes_ordinary_timers() {
    let directory = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(directory.path());
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
    let mut dev = Dev::start_managed(&app, &cli);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap();
    let response = client.get(&dev.url).send().await.unwrap();
    assert_eq!(
        response.status(),
        200,
        "the shared runtime must serve the unchanged installed template"
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

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_package_reload_prepares_only_its_layer_over_the_held_runtime_baseline() {
    let directory = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(directory.path());
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
    let mut dev = Dev::start_managed(&app, &cli);
    let source = app.join("Application.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(
        &source,
        original.replace("Hello from Isoplane!", "Prepared over the baseline"),
    )
    .unwrap();
    dev.wait_event("reloaded");
    let diagnostics = dev.diagnostics();
    let layered: Vec<&str> = diagnostics
        .lines()
        .filter(|line| line.contains("local preparation over the runtime baseline ready"))
        .collect();
    assert_eq!(
        layered.len(),
        2,
        "the first preparation and the reload must each prepare only the package's layer:\n{diagnostics}"
    );
    assert!(
        layered[1].contains("\"compiled\":false")
            && !diagnostics.contains("local native compilation ready"),
        "a package reload must compile nothing:\n{diagnostics}"
    );
    let body = reqwest::get(&dev.url).await.unwrap().text().await.unwrap();
    assert_eq!(
        body, "Prepared over the baseline",
        "the reload must serve the edited package"
    );
    dev.stop();
}

#[tokio::test]
async fn an_installed_template_streams_an_upload_through_the_shared_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(directory.path());
    let source = app.join("Application.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(
        &source,
        original.replace(
            "return app;",
            "app.MapPost(\"/upload\", async (Microsoft.AspNetCore.Http.HttpRequest request) => { long total = 0; var buffer = new byte[4096]; int read; while ((read = await request.Body.ReadAsync(buffer)) != 0) total += read; return $\"received:{total}\"; }); return app;",
        ),
    )
    .unwrap();
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
    let mut dev = Dev::start_managed(&app, &cli);
    let body = vec![b'x'; 3 * 1024 * 1024];
    let response = reqwest::Client::new()
        .post(format!("{}/upload", dev.url))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "the shared runtime must route an upload to the package's ASP.NET endpoint"
    );
    assert_eq!(
        response.text().await.unwrap(),
        "received:3145728",
        "the package's endpoint must read the whole body the runtime streams to it"
    );
    dev.stop();
}

fn write_debug_application(app: &std::path::Path, mode: &str) -> std::path::PathBuf {
    let source = app.join("Application.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    let modified = original.replace(
        "app.MapGet(\"/\", () => \"Hello from Isoplane!\");",
        "app.MapGet(\"/\", Handle);\n        app.MapGet(\"/warmup\", async () => { await Task.Delay(1); return \"warm\"; });",
    ).replace("WarmupAsync(\"/\")", "WarmupAsync(\"/warmup\")");
    let position = modified.rfind('}').unwrap();
    let handler = if mode == "features" {
        include_str!("common/debug_features.cs")
    } else if matches!(mode, "async-stack" | "async-optimized") {
        include_str!("common/debug_async.cs")
    } else if mode == "library" {
        "    private static string Handle() => DebugLibrary.Calculator.Work(41).ToString();\n"
    } else if mode == "protocol" {
        include_str!("common/debug_protocol.cs")
    } else if mode == "watchpoints" {
        include_str!("common/debug_watchpoints.cs")
    } else {
        r#"
    private static async Task<string> Handle()
    {
        int answer = 42;
        Task delay = Task.Delay(100);
        if (delay.IsCompleted) throw new InvalidOperationException("host timer must be incomplete before awaiting");
        await delay;
        bool resumed = true;
        return resumed ? answer.ToString() : throw new InvalidOperationException("continuation did not run");
    }
"#
    };
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
    source
}

fn write_debug_library(directory: &std::path::Path, app: &std::path::Path) -> std::path::PathBuf {
    let library = directory.join("library");
    std::fs::create_dir(&library).unwrap();
    std::fs::write(library.join("Library.csproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework><DebugType>portable</DebugType><Optimize>false</Optimize></PropertyGroup></Project>").unwrap();
    let library_source = library.join("Calculator.cs");
    std::fs::write(&library_source, include_str!("common/debug_library.cs")).unwrap();
    let project = std::fs::read_dir(app)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "csproj")
        })
        .unwrap();
    let xml = std::fs::read_to_string(&project).unwrap().replace("</Project>",
        "<ItemGroup><ProjectReference Include=\"../library/Library.csproj\" /></ItemGroup></Project>");
    std::fs::write(project, xml).unwrap();
    library_source
}

fn start_debug_session(app: &std::path::Path, cli: &std::path::Path, mode: &str) -> Dev {
    if mode == "dotnet" {
        let mut command = std::process::Command::new("dotnet");
        let mut paths = vec![cli.parent().unwrap().to_owned()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command
            .args([
                "run",
                "--no-launch-profile",
                "-c",
                "Debug",
                "-p:IsoplaneDebug=true",
                "--",
                "--json",
                "--port",
                "0",
            ])
            .current_dir(app)
            .env("MSBUILDTERMINALLOGGER", "off")
            .env("ISOPLANE_CLI", cli)
            .env("PATH", std::env::join_paths(paths).unwrap());
        Dev::spawn(&mut command, std::time::Duration::from_secs(600), false)
    } else {
        Dev::start_managed_with_args(app, cli, &["--debug", "--no-watch"])
    }
}

fn installed_debug_acceptance(mode: &str) {
    let python = isoplane_fixtures::debugger_python();
    let directory = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(directory.path());
    if mode == "async-optimized" {
        let project = app.join("Template-App.csproj");
        let xml = std::fs::read_to_string(&project).expect("read the async fixture project");
        std::fs::write(
            &project,
            xml.replace(
                "</Project>",
                "<PropertyGroup><Optimize>true</Optimize></PropertyGroup></Project>",
            ),
        )
        .expect("enable optimized async state machines in the fixture");
    }
    let source = write_debug_application(&app, mode);
    let debug_source = if mode == "library" {
        write_debug_library(directory.path(), &app)
    } else {
        source
    };
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
    let mut dev =
        (!matches!(mode, "launch" | "reload")).then(|| start_debug_session(&app, &cli, mode));
    let driver =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/common/debug_driver.py");
    let mut command = std::process::Command::new(python);
    command.arg(driver).arg(&cli);
    if let Some(dev) = &dev {
        let metadata = dev
            .debugger
            .as_ref()
            .expect("debug CLI must report its relay and package");
        command
            .arg(metadata["endpoint"].as_str().unwrap())
            .arg(&dev.url)
            .arg(debug_source)
            .arg(dev.pid().to_string())
            .arg(mode)
            .arg(metadata["package"].as_str().unwrap());
    } else {
        command
            .args(["", ""])
            .arg(debug_source)
            .args(["0", mode])
            .arg(&app);
    }
    let result = debugger::drive(dev.as_mut(), &mut command);
    assert!(
        result.status.success(),
        "installed debug {mode} acceptance failed: {}{}\nRuntime output:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
        dev.as_ref().map(Dev::diagnostics).unwrap_or_default()
    );
    if let Some(dev) = &mut dev {
        dev.wait_exit();
    }
    assert!(
        !app.join(".isoplane/state").exists(),
        "a disposable debugger session must not leave runtime state in the application's persistent directory"
    );
    assert!(
        std::fs::read_dir(app.join(".isoplane/debug"))
            .unwrap()
            .next()
            .is_none(),
        "debugger exit must remove the entire owned session, including runtime scratch: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn source_reloads_preserve_the_paused_request_and_rebind_the_next_request() {
    installed_debug_acceptance("reload");
}

#[test]
fn an_editor_launches_and_debugs_multiple_requests_without_a_manual_port() {
    installed_debug_acceptance("launch");
}

#[test]
fn one_debugger_connection_preserves_breakpoints_across_requests() {
    installed_debug_acceptance("requests");
}

#[test]
fn installed_debugging_inspects_a_local_and_steps_across_a_host_await() {
    installed_debug_acceptance("continue");
}

#[test]
fn interpreted_debugging_reconstructs_suspended_async_callers() {
    installed_debug_acceptance("async-stack");
}

#[test]
fn optimized_async_callers_keep_editable_state_and_watchpoints() {
    installed_debug_acceptance("async-optimized");
}

#[test]
fn stopping_the_installed_cli_terminates_a_paused_debug_session() {
    installed_debug_acceptance("stop");
}

#[test]
fn installed_debugging_evaluates_and_edits_values_with_conditional_breakpoints() {
    installed_debug_acceptance("features");
}

#[test]
fn installed_watchpoints_observe_managed_field_and_array_changes_between_source_lines() {
    installed_debug_acceptance("watchpoints");
}

#[test]
fn installed_debugger_rejects_invalid_watchpoint_commands_without_losing_the_session() {
    installed_debug_acceptance("protocol");
}

#[test]
fn dotnet_run_starts_an_inspectable_interpreted_debug_session() {
    installed_debug_acceptance("dotnet");
}

#[test]
fn installed_debugging_binds_source_and_inspects_locals_in_referenced_projects() {
    installed_debug_acceptance("library");
}
