use std::path::{Path, PathBuf};
use std::process::Command;

fn stage_packages(destination: &Path) -> PathBuf {
    let source = std::env::var_os("ISOPLANE_TEST_PACKAGE_FEED")
        .map(PathBuf::from)
        .expect("run tests through Task to select the prepared SDK package feed");
    let packages = destination.join("packages");
    std::fs::create_dir_all(&packages).expect("create the isolated package feed");
    for entry in std::fs::read_dir(&source).expect("read the prepared SDK package feed") {
        let entry = entry.expect("read a prepared package");
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "nupkg")
        {
            std::fs::copy(entry.path(), packages.join(entry.file_name()))
                .expect("copy a prepared SDK package");
        }
    }
    packages
}

/// The release version `Directory.Build.props` declares: the version every
/// first-party package packs as unless a build overrides it.
pub fn release_version() -> String {
    let path = super::repo_root().join("Directory.Build.props");
    let props = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} declares the release version: {e}", path.display()));
    let (_, rest) = props
        .split_once("<Version>")
        .expect("Directory.Build.props declares <Version>");
    let (version, _) = rest
        .split_once("</Version>")
        .expect("Directory.Build.props closes its <Version> element");
    version.trim().to_owned()
}

/// Installs the packaged ASP.NET template into an isolated hive and creates a
/// consumer for its managed application configurations. The returned project resolves
/// Isoplane packages exclusively from its local feed.
pub fn aspnet_project(destination: &Path) -> PathBuf {
    let root = super::repo_root();
    let packages = stage_packages(destination);
    std::fs::copy(root.join("global.json"), destination.join("global.json"))
        .expect("pin the template consumer SDK");
    let hive = destination.join("template-hive");
    run(Command::new("dotnet")
        .args(["new", "install"])
        .arg(packages.join(format!("Isoplane.Templates.{}.nupkg", release_version())))
        .arg("--debug:custom-hive")
        .arg(&hive));
    let app = destination.join("app");
    run(Command::new("dotnet")
        .args(["new", "isoplane-web", "--name", "Template-App"])
        .arg("--output")
        .arg(&app)
        .arg("--debug:custom-hive")
        .arg(&hive)
        .current_dir(destination));
    let config = app.join("nuget.config");
    run(Command::new("dotnet")
        .args(["nuget", "add", "source"])
        .arg(&packages)
        .args(["--name", "template-packages", "--configfile"])
        .arg(&config));
    let xml = std::fs::read_to_string(&config).expect("read generated package sources");
    let xml = xml.replace(
        "</configuration>",
        &format!(
            "<config><add key=\"globalPackagesFolder\" value=\"{}\" /></config>\n\
             <packageSourceMapping>\n\
             <packageSource key=\"template-packages\"><package pattern=\"Isoplane.*\" /></packageSource>\n\
             <packageSource key=\"nuget\"><package pattern=\"*\" /></packageSource>\n\
             </packageSourceMapping></configuration>",
            destination.join("nuget").to_string_lossy().replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;")
        ),
    );
    std::fs::write(config, xml).expect("isolate the template package cache and source mapping");
    app
}

fn run(command: &mut Command) {
    let result = command.output().unwrap_or_else(|error| {
        panic!("dotnet SDK is required for template installation: {error}")
    });
    assert!(
        result.status.success(),
        "template consumer setup must succeed ({command:?}):\n{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// The debugger test interpreter prepared by Task.
pub fn debugger_python() -> PathBuf {
    std::env::var_os("ISOPLANE_TEST_DEBUGGER_PYTHON")
        .map(PathBuf::from)
        .expect("run tests through Task to select the prepared debugger Python environment")
}

/// Installs the packaged managed CLI with native local support into a local tool directory.
pub fn managed_cli(destination: &Path, app: &Path) -> PathBuf {
    stage_packages(destination);
    let tools = destination.join("tools");
    let mut install = Command::new("dotnet");
    if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) && std::env::var_os("DOTNET_RUNTIME_ID").is_none()
    {
        // Ubuntu's SDK needs its portable RID to locate packaged tool settings.
        install.env("DOTNET_RUNTIME_ID", "linux-x64");
    }
    run(install
        .args(["tool", "install", "Isoplane.Cli", "--version"])
        .arg(release_version())
        .arg("--tool-path")
        .arg(&tools)
        .arg("--configfile")
        .arg(app.join("nuget.config"))
        .current_dir(destination));
    tools.join(format!("isoplane{}", std::env::consts::EXE_SUFFIX))
}
