//! Installed projects select their sandbox profile through ordinary MSBuild configuration.

#[path = "common/dev.rs"]
pub mod common;

use std::process::Command;

#[test]
fn installed_sandbox_projects_do_not_advertise_clr_hot_reload() {
    let directory = tempfile::tempdir().expect("isolate installed SDK capability evaluation");
    let app = wisp_fixtures::aspnet_project(directory.path());
    for configuration in ["WispInterpretedDebug", "WispNativeAotRelease"] {
        let output = Command::new("dotnet")
            .args(["msbuild", "-getItem:ProjectCapability"])
            .arg(format!("-p:Configuration={configuration}"))
            .env(
                "WISP_GUEST_RUNTIME_FEED",
                wisp_fixtures::guest_runtime_feed(),
            )
            .current_dir(&app)
            .output()
            .expect("evaluate installed .NET project capabilities");
        assert!(
            output.status.success(),
            "project capability evaluation must succeed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let evaluation: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("MSBuild returns capability JSON");
        let capabilities = evaluation["Items"]["ProjectCapability"]
            .as_array()
            .expect("project capabilities are an item list");
        assert!(
            capabilities
                .iter()
                .all(|item| item["Identity"] != "SupportsHotReload"),
            "sandbox builds must not advertise unsupported CLR hot reload to .NET tooling"
        );
    }
}

#[tokio::test]
async fn dotnet_run_uses_the_profile_declared_in_the_project_body() {
    let directory = tempfile::tempdir().expect("isolate the installed project workflow");
    let app = wisp_fixtures::aspnet_project(directory.path());
    let library = directory.path().join("CustomerLibrary");
    std::fs::create_dir(&library).expect("create an ordinary existing class library");
    std::fs::write(
        library.join("CustomerLibrary.csproj"),
        r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>"#,
    )
    .expect("write the existing library without WISP SDK dependencies");
    std::fs::write(
        library.join("Greeting.cs"),
        "namespace CustomerLibrary; public static class Greeting { public static string Message => \"Hello from the existing library!\"; }",
    )
    .expect("write the ordinary customer application logic");
    let project = app.join("Template-App.csproj");
    let project_text = std::fs::read_to_string(&project).expect("read the generated project");
    std::fs::write(
        &project,
        project_text.replace(
            "</Project>",
            r#"<PropertyGroup><RuntimeFrameworkVersion>10.0.12</RuntimeFrameworkVersion></PropertyGroup><ItemGroup><ProjectReference Include="../CustomerLibrary/CustomerLibrary.csproj" /></ItemGroup></Project>"#,
        ),
    )
    .expect("onboard existing code through a standard project reference");
    let source = app.join("Application.cs");
    let code = std::fs::read_to_string(&source).expect("read the generated application");
    let route = code.replace(
        "() => \"Hello from Isoplane!\"",
        "() => { app.Logger.LogInformation(\"guest-log-marker\"); return CustomerLibrary.Greeting.Message; }",
    );
    std::fs::write(
        &source,
        format!("using Microsoft.Extensions.Logging;\n{route}"),
    )
    .expect("exercise ordinary application logging inside both guests");
    let cli = wisp_fixtures::managed_cli(directory.path(), &app);
    for (configuration, runtime, aot) in [
        ("WispInterpretedDebug", "Mono", "false"),
        ("WispNativeAotRelease", "NativeAOT", "true"),
    ] {
        let evaluated = Command::new("dotnet")
            .args([
                "msbuild",
                "-getProperty:WispGuestRuntime,PublishAot,RuntimeIdentifier",
            ])
            .arg(format!("-p:Configuration={configuration}"))
            .env(
                "WISP_GUEST_RUNTIME_FEED",
                wisp_fixtures::guest_runtime_feed(),
            )
            .current_dir(&app)
            .output()
            .expect("evaluate the installed project with the .NET SDK");
        assert!(
            evaluated.status.success(),
            "project evaluation must succeed: {}",
            String::from_utf8_lossy(&evaluated.stdout)
        );
        let properties: serde_json::Value = serde_json::from_slice(&evaluated.stdout)
            .expect("MSBuild property queries produce JSON");
        assert_eq!(
            properties["Properties"]["WispGuestRuntime"], runtime,
            "project-body configuration must select the actual sandbox runtime"
        );
        assert_eq!(
            properties["Properties"]["PublishAot"], aot,
            "the ordinary PublishAot property must remain visible to tooling"
        );
        assert_eq!(
            properties["Properties"]["RuntimeIdentifier"], "wasi-wasm",
            "both execution modes target WebAssembly"
        );
        let mut running = start_dotnet(&app, &cli, configuration, false);
        let response = reqwest::get(&running.url)
            .await
            .expect("the published sandbox must accept HTTP");
        assert!(
            response.status().is_success(),
            "the project-selected sandbox must serve successful requests"
        );
        let body = response.text().await.expect("read the sandbox response");
        assert!(
            body.contains("Hello from the existing library!"),
            "dotnet run must execute the referenced existing application library"
        );
        running.wait_for("guest-log-marker");
        running.stop();
    }
}

fn start_dotnet(
    project: &std::path::Path,
    cli: &std::path::Path,
    configuration: &str,
    watch: bool,
) -> common::Dev {
    let mut command = Command::new("dotnet");
    let mut paths = vec![cli
        .parent()
        .expect("installed CLI has a tools directory")
        .to_owned()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    if watch {
        command.args(["watch", "--non-interactive", "--no-hot-reload"]);
    }
    command
        .args([
            "run",
            "--no-launch-profile",
            "--configuration",
            configuration,
            "--",
            "--json",
            "--port",
            "0",
        ])
        .current_dir(project)
        .env("WISP_CLI", cli)
        .env(
            "PATH",
            std::env::join_paths(paths).expect("tool paths contain no path separators"),
        );
    common::Dev::spawn(&mut command, std::time::Duration::from_secs(900), false)
}

#[tokio::test]
async fn dotnet_watch_rebuilds_the_guest_after_a_compilation_error_is_repaired() {
    let directory = tempfile::tempdir().expect("isolate the installed watch workflow");
    let app = wisp_fixtures::aspnet_project(directory.path());
    let cli = wisp_fixtures::managed_cli(directory.path(), &app);
    let mut running = start_dotnet(&app, &cli, "WispInterpretedDebug", true);
    let source = app.join("Application.cs");
    let original = std::fs::read_to_string(&source).expect("read the generated application");
    std::fs::write(&source, "this is not valid C#").expect("introduce a compile failure");
    running.wait_for("error CS");
    std::fs::write(
        &source,
        original.replace("Hello from Isoplane!", "Repaired guest"),
    )
    .expect("repair the application");
    let ready = running.wait_event("ready");
    let address = ready["url"]
        .as_str()
        .expect("the restarted runtime reports its address");
    let body = reqwest::get(address)
        .await
        .expect("the rebuilt guest must accept requests")
        .text()
        .await
        .expect("read the rebuilt guest response");
    assert_eq!(
        body, "Repaired guest",
        "watch must rebuild the sandbox module from the repaired source before restarting it"
    );
    running.stop();
}
