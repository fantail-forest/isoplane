//! Installed projects build packages and run them on their SDK's shared runtime.

#[path = "common/dev.rs"]
pub mod common;

use isoplane_test_support::{directories_under, run_successfully};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

/// The files below `directory`, by their path relative to it.
fn files_under(directory: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut directories = directories_under(directory);
    directories.push(directory.to_owned());
    directories
        .iter()
        .flat_map(|directory| std::fs::read_dir(directory).expect("list a skill directory"))
        .map(|entry| entry.expect("read a skill directory entry").path())
        .filter(|path| path.is_file())
        .map(|path| {
            let relative = path
                .strip_prefix(directory)
                .expect("the file is below the directory")
                .to_owned();
            (relative, std::fs::read(&path).expect("read a skill file"))
        })
        .collect()
}

/// Fails unless `actual` holds the files of `expected` with the same content. `what` states
/// the equality, and a failure names the files or the first file that break it.
fn assert_same_files(
    actual: &BTreeMap<PathBuf, Vec<u8>>,
    expected: &BTreeMap<PathBuf, Vec<u8>>,
    what: &str,
) {
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "{what}: the files differ"
    );
    for (file, content) in expected {
        assert!(
            actual[file] == *content,
            "{what}: {} differs",
            file.display()
        );
    }
}

#[test]
fn a_template_project_carries_the_skill_install_writes_for_its_packages() {
    for (template, application) in [
        ("isoplane-web", ""),
        ("isoplane-handler", "src/Tests.Template-App"),
        ("isoplane-mcpserver", "src/Tools.Template-App"),
        ("isoplane-webapi", "src/Todos.Template-App"),
        ("isoplane-webapp", "src/Notes.Template-App"),
    ] {
        let directory = tempfile::tempdir().expect("isolate the template project");
        let solution = isoplane_fixtures::template_project(directory.path(), template);
        let agents = solution.join(".agents/skills/isoplane");
        let shipped = files_under(&solution.join(".claude/skills/isoplane"));
        assert_same_files(
            &files_under(&agents),
            &shipped,
            &format!("{template} ships in .agents/skills what it ships in .claude/skills"),
        );

        // The project sits below the root of a repository, where the command would install a
        // skill of its own, and has lost what its restore wrote.
        std::fs::create_dir(directory.path().join(".git")).expect("mark the repository's root");
        std::fs::remove_dir_all(solution.join(application).join("obj"))
            .expect("remove what the restore wrote");
        std::fs::remove_dir_all(&agents).expect("remove one shipped skill's files");
        std::fs::create_dir(&agents).expect("keep the directory that says where the skill is");
        run_successfully(
            Command::new(isoplane_fixtures::managed_cli())
                .args(["skills", "install", "--dot-agents", "--project"])
                .arg(solution.join(application))
                .env("ISOPLANE_TELEMETRY_OPTOUT", "1"),
            &format!("install the skill for {template}'s packages"),
        );
        assert_same_files(
            &files_under(&agents),
            &shipped,
            &format!(
                "isoplane skills install restores {template} and writes beside the skill it ships what it ships"
            ),
        );
        assert!(
            !directory.path().join(".agents").exists(),
            "{template}'s skill is written where the template put it, not again at the repository's root"
        );
    }
}

#[test]
fn a_template_leaves_out_the_skills_directory_its_creator_switched_off() {
    for template in [
        "isoplane-web",
        "isoplane-handler",
        "isoplane-mcpserver",
        "isoplane-webapi",
        "isoplane-webapp",
    ] {
        for (option, kept, left_out) in [
            ("dot-claude", ".agents", ".claude"),
            ("dot-agents", ".claude", ".agents"),
        ] {
            let project = isoplane_fixtures::template_without(template, option);
            assert!(
                project
                    .join(kept)
                    .join("skills/isoplane/SKILL.md")
                    .is_file(),
                "{template} created with --{option} false keeps the skill in {kept}"
            );
            assert!(
                !project.join(left_out).exists(),
                "{template} created with --{option} false has no {left_out}"
            );
        }
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
            && diagnostics.contains(" compiled=false"),
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
