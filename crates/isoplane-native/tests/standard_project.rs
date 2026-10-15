//! Installed projects build packages and run them on their SDK's shared runtime.

#[path = "common/dev.rs"]
pub mod common;

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
        let output = Command::new("dotnet")
            .arg("build")
            .current_dir(&solution)
            .output()
            .expect("build the generated solution with the .NET SDK");
        assert!(
            output.status.success(),
            "{template} must build under its prepared name, which its files carry and its namespaces derive from:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn installed_sandbox_projects_do_not_advertise_clr_hot_reload() {
    let directory = tempfile::tempdir().expect("isolate installed SDK capability evaluation");
    let app = isoplane_fixtures::handler_project(directory.path());
    for configuration in ["Debug", "Release"] {
        let output = Command::new("dotnet")
            .args(["msbuild", "-getItem:ProjectCapability"])
            .arg(format!("-p:Configuration={configuration}"))
            .env(
                "ISOPLANE_GUEST_RUNTIME_FEED",
                isoplane_fixtures::guest_runtime_feed(),
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
async fn dotnet_run_executes_an_application_package_with_an_existing_library() {
    let directory = tempfile::tempdir().expect("isolate the installed project workflow");
    let app = isoplane_fixtures::handler_project(directory.path());
    let library = directory.path().join("CustomerLibrary");
    std::fs::create_dir(&library).expect("create an ordinary existing class library");
    std::fs::write(
        library.join("CustomerLibrary.csproj"),
        r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>"#,
    )
    .expect("write the existing library without Isoplane SDK dependencies");
    std::fs::write(
        library.join("Greeting.cs"),
        "namespace CustomerLibrary; public static class Greeting { public static string Message => \"Hello from the existing library!\"; }",
    )
    .expect("write the ordinary customer application logic");
    let project = app.join("HandlerFixture.csproj");
    let project_text = std::fs::read_to_string(&project).expect("read the fixture project");
    std::fs::write(
        &project,
        project_text.replace(
            "</Project>",
            r#"<ItemGroup><ProjectReference Include="../CustomerLibrary/CustomerLibrary.csproj" /></ItemGroup></Project>"#,
        ),
    )
    .expect("onboard existing code through a standard project reference");
    std::fs::write(
        app.join("Handler.cs"),
        r#"using Isoplane;

public static class Handler
{
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request)
    {
        Console.WriteLine("guest-log-marker");
        return Task.FromResult(Response.Text(CustomerLibrary.Greeting.Message));
    }
}
"#,
    )
    .expect("exercise ordinary application logging inside the package");
    let cli = isoplane_fixtures::managed_cli();
    {
        let configuration = "Debug";
        let evaluated = Command::new("dotnet")
            .args([
                "msbuild",
                "-getProperty:IsoplaneManagedBundleBuild,RuntimeIdentifier",
            ])
            .arg(format!("-p:Configuration={configuration}"))
            .env(
                "ISOPLANE_GUEST_RUNTIME_FEED",
                isoplane_fixtures::guest_runtime_feed(),
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
            properties["Properties"]["IsoplaneManagedBundleBuild"], "true",
            "an ordinary SDK application must build a package"
        );
        assert_eq!(
            properties["Properties"]["RuntimeIdentifier"], "wasi-wasm",
            "an application package must resolve dependencies for the guest runtime"
        );
        let mut running = start_dotnet(&app, &cli, configuration, false, false, &[]);
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
    let mut found = Vec::new();
    let mut pending = vec![project.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
                });
            if named && path.join("manifest.bin").is_file() {
                found.push(path);
            } else {
                pending.push(path);
            }
        }
    }
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
    for (name, value) in settings {
        command.env(format!("ISOPLANE_APP_{name}"), value);
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
