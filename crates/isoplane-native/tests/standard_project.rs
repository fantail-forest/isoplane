//! Installed projects build packages and run them on their SDK's shared runtime.

#[path = "common/dev.rs"]
pub mod common;

use isoplane_test_support::{directories_under, run_successfully};
use std::process::Command;

#[test]
fn templates_build_under_names_that_are_not_identifiers() {
    // Task generates and restores each template under a name chosen to break
    // namespace resolution; taskfiles/fixtures.yml says why. Building it is the check.
    for template in [
        "isoplane-web",
        "isoplane-handler",
        "isoplane-mcpserver",
        "isoplane-webapi",
        "isoplane-webapp",
    ] {
        let directory = tempfile::tempdir().expect("isolate the template build");
        let solution = isoplane_fixtures::template_project(directory.path(), template);
        run_successfully(
            Command::new("dotnet").arg("build").current_dir(&solution),
            &format!("build {template} under its prepared name, which its files carry and its namespaces derive from"),
        );
    }
}

#[test]
fn installed_sandbox_projects_do_not_advertise_clr_hot_reload() {
    let directory = tempfile::tempdir().expect("isolate installed SDK capability evaluation");
    let app = isoplane_fixtures::handler_project(directory.path());
    for configuration in ["Debug", "Release"] {
        let output = run_successfully(
            Command::new("dotnet")
                .args(["msbuild", "-getItem:ProjectCapability"])
                .arg(format!("-p:Configuration={configuration}"))
                .env(
                    "ISOPLANE_GUEST_RUNTIME_FEED",
                    isoplane_fixtures::guest_runtime_feed(),
                )
                .current_dir(&app),
            "evaluate the installed project's capabilities with the .NET SDK",
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
async fn dotnet_run_executes_an_application_package_with_an_existing_library() {
    let directory = tempfile::tempdir().expect("isolate the installed project workflow");
    let app = isoplane_fixtures::handler_project(directory.path());
    let cli = isoplane_fixtures::managed_cli();
    {
        let configuration = "Debug";
        let evaluated = run_successfully(
            Command::new("dotnet")
                .args([
                    "msbuild",
                    "-getProperty:IsoplaneManagedBundleBuild,RuntimeIdentifier",
                ])
                .arg(format!("-p:Configuration={configuration}"))
                .env(
                    "ISOPLANE_GUEST_RUNTIME_FEED",
                    isoplane_fixtures::guest_runtime_feed(),
                )
                .current_dir(&app),
            "evaluate the installed project with the .NET SDK",
        );
        let properties: serde_json::Value = serde_json::from_slice(&evaluated.stdout)
            .expect("MSBuild property queries produce JSON");
        assert_eq!(
            properties["Properties"]["IsoplaneManagedBundleBuild"], "true",
            "an ordinary SDK application must build a package"
        );
        assert_eq!(
            properties["Properties"]["RuntimeIdentifier"], "wasi-wasm",
            "an application package must resolve dependencies for the guest runtime"
        );
        let mut running = start_dotnet(&app, &cli, configuration, false, false, &[]);
        let response = reqwest::get(format!("{}/greeting", running.url))
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

#[tokio::test]
async fn an_unchanged_interpreted_run_reuses_its_prepared_template() {
    let directory = tempfile::tempdir().expect("isolate the incremental installed project journey");
    let app = isoplane_fixtures::handler_project(directory.path());
    let cli = isoplane_fixtures::managed_cli();
    let mut first = start_dotnet(&app, &cli, "Debug", false, false, &[]);
    first.stop();
    std::fs::write(
        app.join("Handler.cs"),
        "invalid C# that must not be compiled",
    )
    .expect("change source after building the application");
    let prepared = application_layers(&app);
    assert_eq!(prepared.len(), 1, "a run keeps its prepared application");
    let mut second = start_dotnet(&app, &cli, "Debug", false, true, &[]);
    let diagnostics = second.diagnostics();
    assert!(
        diagnostics.contains("reusing the local runtime baseline")
            && diagnostics.contains("reusing the prepared application")
            && diagnostics.contains("\"compiled\":false"),
        "an unchanged run must reuse its prepared application and native compilation:\n{diagnostics}"
    );
    let response = reqwest::get(&second.url)
        .await
        .expect("request the reused application");
    assert_eq!(
        response.status(),
        200,
        "a reused interpreted preparation must serve requests"
    );
    assert_eq!(
        response
            .text()
            .await
            .expect("read the reused application response"),
        "Hello from Isoplane!",
        "the reused preparation must execute the application's package"
    );
    second.stop();
    assert_eq!(application_layers(&app), prepared);

    let mut third = start_dotnet(&app, &cli, "Debug", false, true, &[("Greeting", "changed")]);
    let diagnostics = third.diagnostics();
    assert!(
        !diagnostics.contains("reusing the prepared application"),
        "a changed setting may have been captured, so the application must be prepared again:\n{diagnostics}"
    );
    third.stop();
    let replaced = application_layers(&app);
    assert_eq!(
        replaced.len(),
        1,
        "the earlier layer must be removed: {replaced:?}"
    );
    assert_ne!(replaced, prepared);
}

/// The application layers published under the project: directories named by an identity that
/// hold a prepared manifest.
fn application_layers(project: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found: Vec<_> = directories_under(project)
        .into_iter()
        .filter(|path| {
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
                });
            named && path.join("manifest.bin").is_file()
        })
        .collect();
    found.sort();
    found
}

fn start_dotnet(
    project: &std::path::Path,
    cli: &std::path::Path,
    configuration: &str,
    watch: bool,
    no_build: bool,
    settings: &[(&str, &str)],
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
    command.arg("run");
    if no_build {
        command.arg("--no-build");
    }
    command
        .args([
            "--no-launch-profile",
            "--configuration",
            configuration,
            "--",
            "--json",
            "--port",
            "0",
        ])
        .current_dir(project)
        .env("ISOPLANE_CLI", cli)
        .env(
            "XDG_DATA_HOME",
            project
                .parent()
                .expect("fixture project has a root")
                .join("user-data"),
        )
        .env(
            "PATH",
            std::env::join_paths(paths).expect("tool paths contain no path separators"),
        );
    if !settings.is_empty() {
        let settings: serde_json::Map<String, serde_json::Value> = settings
            .iter()
            .map(|(name, value)| (name.to_string(), serde_json::Value::from(*value)))
            .collect();
        command.args([
            "--settings",
            &serde_json::Value::Object(settings).to_string(),
        ]);
    }
    common::Dev::spawn(&mut command, std::time::Duration::from_secs(900), false)
}

#[tokio::test]
async fn dotnet_watch_rebuilds_the_guest_after_a_compilation_error_is_repaired() {
    let directory = tempfile::tempdir().expect("isolate the installed watch workflow");
    let app = isoplane_fixtures::handler_project(directory.path());
    let cli = isoplane_fixtures::managed_cli();
    let mut running = start_dotnet(&app, &cli, "Debug", true, false, &[]);
    let source = app.join("Handler.cs");
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
