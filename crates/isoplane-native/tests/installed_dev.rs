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
        "the installed ASP.NET template must serve successfully through the local sandbox:\n{}",
        dev.diagnostics()
    );
    assert_eq!(
        response.text().await.unwrap(),
        "Hello from Isoplane!",
        "the generated ASP.NET route must execute without copied interop boilerplate"
    );
    let source = app.join("Program.cs");
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
        "the shared runtime must serve the unchanged installed template:\n{}",
        dev.diagnostics()
    );
    assert_eq!(
        response.text().await.unwrap(),
        "Hello from Isoplane!",
        "Mono must invoke the generated ASP.NET exports through the actual sandbox"
    );
    let source = app.join("Program.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(&source, original.replace("await app.RunAsync();", "app.MapGet(\"/async\", async () => { await System.Threading.Tasks.Task.Delay(1); return \"async-ready\"; });\nawait app.RunAsync();")).unwrap();
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
    let source = app.join("Program.cs");
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
    let source = app.join("Program.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    std::fs::write(
        &source,
        original.replace(
            "await app.RunAsync();",
            "app.MapPost(\"/upload\", async (Microsoft.AspNetCore.Http.HttpRequest request) => { long total = 0; var buffer = new byte[4096]; int read; while ((read = await request.Body.ReadAsync(buffer)) != 0) total += read; return $\"received:{total}\"; });\nawait app.RunAsync();",
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
        "the shared runtime must route an upload to the package's ASP.NET endpoint:\n{}",
        dev.diagnostics()
    );
    assert_eq!(
        response.text().await.unwrap(),
        "received:3145728",
        "the package's endpoint must read the whole body the runtime streams to it"
    );
    dev.stop();
}

fn write_debug_application(app: &std::path::Path, mode: &str) -> std::path::PathBuf {
    let source = app.join("Program.cs");
    let original = std::fs::read_to_string(&source).unwrap();
    let modified = original.replace(
        "app.MapGet(\"/\", () => \"Hello from Isoplane!\");",
        "app.MapGet(\"/\", DebugFixture.Application.Handle);\napp.MapGet(\"/warmup\", async () => { await Task.Delay(1); return \"warm\"; });",
    ).replace("WarmupAsync(\"/\")", "WarmupAsync(\"/warmup\")");
    let modified = if mode == "detach-failure" {
        modified.replace("app.MapGet(\"/\", DebugFixture.Application.Handle);", "if (!System.Diagnostics.Debugger.IsAttached) throw new InvalidOperationException(\"ordinary runtime preparation rejected\");\napp.MapGet(\"/\", DebugFixture.Application.Handle);")
    } else {
        modified
    };
    let handler = if mode == "library" {
        "    internal static string Handle() => DebugLibrary.Calculator.Work(41).ToString();\n"
    } else {
        r#"
    internal static async Task<string> Handle()
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
    let detached;
    let handler = if matches!(mode, "detach" | "terminal-detach") {
        detached = handler.replace(
            "return resumed ?",
            "return resumed && !System.Diagnostics.Debugger.IsAttached ?",
        );
        detached.as_str()
    } else {
        handler
    };
    // Type declarations follow a program's top-level statements.
    std::fs::write(
        &source,
        format!("{modified}\nnamespace DebugFixture\n{{\n    internal static class Application\n    {{\n{handler}    }}\n}}\n"),
    )
    .unwrap();
    source
}

fn write_debug_library(directory: &std::path::Path, app: &std::path::Path) -> std::path::PathBuf {
    let library = directory.join("library");
    std::fs::create_dir(&library).unwrap();
    std::fs::copy(app.join("nuget.config"), library.join("nuget.config"))
        .expect("share the application package sources with its referenced project");
    std::fs::write(library.join("Library.csproj"),
        "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net10.0</TargetFramework><DebugType>portable</DebugType><Optimize>false</Optimize></PropertyGroup><ItemGroup><EmbeddedResource Include=\"payload.bin\" /></ItemGroup></Project>").unwrap();
    // Metadata replies include the assembly image and must survive socket backpressure.
    std::fs::write(library.join("payload.bin"), vec![0_u8; 8 * 1024 * 1024]).unwrap();
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
    } else if matches!(mode, "detach" | "detach-failure") {
        Dev::start_managed_with_args(app, cli, &["--debug"])
    } else {
        Dev::start_managed_with_args(app, cli, &["--debug", "--no-watch"])
    }
}

fn installed_debug_acceptance(mode: &str) {
    let python = isoplane_fixtures::debugger_python();
    let directory = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::aspnet_project(directory.path());
    let source = write_debug_application(&app, mode);
    let debug_source = if mode == "library" {
        write_debug_library(directory.path(), &app)
    } else {
        source
    };
    let cli = isoplane_fixtures::managed_cli(directory.path(), &app);
    let mut dev = (!matches!(
        mode,
        "launch" | "reload" | "terminal-launch" | "terminal-detach"
    ))
    .then(|| start_debug_session(&app, &cli, mode));
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
fn moving_execution_preserves_locals_and_rejects_stale_statement_targets() {
    debugger::prepared_acceptance("statements");
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
fn value_task_async_callers_preserve_saved_values_and_watchpoints() {
    debugger::prepared_acceptance("async-valuetask");
}

#[test]
fn optimized_value_task_async_callers_preserve_saved_values_and_watchpoints() {
    debugger::prepared_acceptance("async-valuetask-optimized");
}

#[test]
fn pooled_value_task_async_callers_preserve_saved_values_and_watchpoints() {
    debugger::prepared_acceptance("async-pooled");
}

#[test]
fn optimized_pooled_value_task_async_callers_preserve_saved_values_and_watchpoints() {
    debugger::prepared_acceptance("async-pooled-optimized");
}

#[test]
fn pooled_value_task_combinators_keep_callers_and_release_reused_state() {
    debugger::prepared_acceptance("async-pooled-composed");
}

#[test]
fn optimized_pooled_value_task_combinators_keep_callers_and_release_reused_state() {
    debugger::prepared_acceptance("async-pooled-composed-optimized");
}

#[test]
fn interpreted_debugging_reconstructs_suspended_async_callers() {
    debugger::prepared_acceptance("async-stack");
}

#[test]
fn optimized_async_callers_keep_editable_state_and_watchpoints() {
    debugger::prepared_acceptance("async-optimized");
}

#[test]
fn async_composed_callers_preserve_custom_awaits_and_task_combinators() {
    debugger::prepared_acceptance("async-composed");
}

#[test]
fn optimized_async_composed_callers_keep_editable_state_and_watchpoints() {
    debugger::prepared_acceptance("async-composed-optimized");
}

#[test]
fn stopping_the_installed_cli_terminates_a_paused_debug_session() {
    installed_debug_acceptance("stop");
}

#[test]
fn debugging_evaluates_and_edits_values_with_conditional_breakpoints() {
    debugger::prepared_acceptance("features");
}

#[test]
fn filtered_watchpoints_preserve_steps_across_calls_and_local_expiration() {
    debugger::prepared_acceptance("watchpoint-steps");
}

#[test]
fn watchpoints_observe_fields_arrays_and_scoped_locals() {
    debugger::prepared_acceptance("watchpoints");
}

#[test]
fn debugger_rejects_invalid_watchpoint_commands_without_losing_the_session() {
    debugger::prepared_acceptance("protocol");
}

#[test]
fn dotnet_run_starts_an_inspectable_interpreted_debug_session() {
    installed_debug_acceptance("dotnet");
}

#[test]
fn installed_debugging_binds_source_and_inspects_locals_in_referenced_projects() {
    installed_debug_acceptance("library");
}

#[test]
fn uncaught_exception_filter_skips_caught_exceptions_and_reports_unhandled_ones() {
    debugger::prepared_acceptance("uncaught");
}

#[test]
fn detaching_preserves_the_active_request_and_keeps_serving_and_reloading() {
    installed_debug_acceptance("detach");
}

#[test]
fn failed_detach_preparation_leaves_the_paused_guest_inspectable_and_resumable() {
    installed_debug_acceptance("detach-failure");
}

#[test]
fn detaching_between_requests_closes_the_pending_guest_connection() {
    installed_debug_acceptance("detach-idle");
}

#[test]
fn an_editor_terminal_launch_serves_requests_and_stops_with_the_debugger() {
    installed_debug_acceptance("terminal-launch");
}

#[test]
fn an_editor_terminal_survives_detach_and_owns_subsequent_requests_and_reload() {
    installed_debug_acceptance("terminal-detach");
}

#[test]
fn moving_execution_across_awaits_preserves_state_and_allows_repeated_suspension() {
    debugger::prepared_acceptance("statement-awaits");
}

#[test]
fn moving_execution_across_awaits_preserves_optimized_state_machines() {
    debugger::prepared_acceptance("statement-awaits-optimized");
}

#[test]
fn moving_execution_within_handlers_preserves_rethrow_and_finally_continuations() {
    debugger::prepared_acceptance("statement-handlers");
}

#[test]
fn moving_execution_after_hot_reload_preserves_live_locals_and_executes_updated_code() {
    debugger::prepared_acceptance("hotreload");
}

#[test]
fn the_hot_reload_compiler_loads_the_prepared_application_baseline() {
    debugger::compiler_acceptance();
}

#[test]
fn hot_reload_without_symbols_keeps_added_methods_inspectable() {
    debugger::prepared_acceptance("hotreload-without-symbols");
}

#[test]
fn moving_execution_after_hot_reload_preserves_finally_cleanup() {
    debugger::prepared_acceptance("hotreload-try");
}

#[test]
fn moving_execution_after_hot_reload_can_repeat_an_await() {
    debugger::prepared_acceptance("hotreload-async");
}

#[test]
fn moving_execution_in_an_unchanged_handler_survives_other_method_updates() {
    debugger::prepared_acceptance("hotreload-unchanged-handler");
}

#[test]
fn moving_execution_rejects_hot_reload_with_changed_local_storage() {
    debugger::prepared_acceptance("hotreload-layout");
}

#[test]
fn editor_source_edits_can_be_applied_and_reexecuted_in_the_paused_request() {
    debugger::prepared_acceptance("edit-and-continue");
}

#[test]
fn editor_source_edits_can_add_locals_and_repeat_an_await() {
    debugger::prepared_acceptance("edit-and-continue-async");
}

#[test]
fn editor_source_edits_can_add_locals_and_preserve_finally_cleanup() {
    debugger::prepared_acceptance("edit-and-continue-try");
}

#[test]
fn editor_source_edits_leave_a_rejected_handler_edit_usable() {
    debugger::prepared_acceptance("edit-and-continue-handler");
}

#[test]
fn editor_source_edits_keep_frames_and_jump_targets_aligned_after_line_changes() {
    debugger::prepared_acceptance("edit-and-continue-lines");
}

#[test]
fn editor_source_edits_are_adopted_when_stepping_and_continuing() {
    debugger::prepared_acceptance("edit-and-continue-resume");
}

#[test]
fn moving_execution_between_iterator_yields_preserves_values_and_disposal() {
    debugger::prepared_acceptance("statement-iterators");
}

#[test]
fn editor_source_edits_preserve_iterator_values_across_yields_and_awaits() {
    debugger::prepared_acceptance("edit-and-continue-iterators");
}

#[test]
fn editor_source_edits_are_adopted_by_callers_without_repeating_calls() {
    debugger::prepared_acceptance("edit-and-continue-callers");
}

#[test]
fn editor_source_edits_are_adopted_after_class_and_value_type_construction() {
    debugger::prepared_acceptance("edit-and-continue-constructor-callers");
}

#[test]
fn editor_source_edits_apply_while_running_and_preserve_breakpoints() {
    debugger::prepared_acceptance("edit-and-continue-running");
}
