use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Installs the packaged ASP.NET template into an isolated hive and creates a consumer.
/// The returned project resolves Isoplane packages exclusively from its local feed.
pub fn aspnet_project(destination: &Path) -> PathBuf {
    let root = super::repo_root();
    let packages = destination.join("packages");
    for project in [
        "Isoplane.Sdk.csproj",
        "AspNetCore/Isoplane.AspNetCore.csproj",
        "Templates/Isoplane.Templates.csproj",
    ] {
        run(Command::new("dotnet")
            .arg("pack")
            .arg(root.join("guest-sdks/dotnet").join(project))
            .arg("--artifacts-path")
            .arg(destination.join("artifacts"))
            .arg("-o")
            .arg(&packages)
            .arg("--disable-build-servers"));
    }
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
    let mut runtime_mapping = String::new();
    for (name, feed) in [
        ("isoplane-guest-runtime", crate::guest_runtime_feed()),
        ("isoplane-guest-compiler", crate::guest_compiler_feed()),
    ] {
        run(Command::new("dotnet")
            .args(["nuget", "add", "source"])
            .arg(&feed)
            .args(["--name", name, "--configfile"])
            .arg(&config));
        let patterns: String = super::package_ids(&feed)
            .iter()
            .map(|id| format!("<package pattern=\"{id}\" />"))
            .collect();
        runtime_mapping.push_str(&format!(
            "<packageSource key=\"{name}\">{patterns}</packageSource>\n"
        ));
    }
    let xml = std::fs::read_to_string(&config).expect("read generated package sources");
    let xml = xml.replace(
        "</configuration>",
        &format!(
            "<config><add key=\"globalPackagesFolder\" value=\"{}\" /></config>\n\
             <packageSourceMapping>\n\
             <packageSource key=\"template-packages\"><package pattern=\"Isoplane.*\" /><package pattern=\"Isoplane.*\" /></packageSource>\n\
             {runtime_mapping}\
             <packageSource key=\"nuget\"><package pattern=\"*\" /></packageSource>\n\
             <packageSource key=\"dotnet-experimental\"><package pattern=\"*\" /></packageSource>\n\
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

/// Packs and installs the managed CLI with native local support into a local tool directory.
pub fn managed_cli(destination: &Path, app: &Path) -> PathBuf {
    let packages = destination.join("packages");
    run(Command::new("dotnet")
        .arg("pack")
        .arg(super::repo_root().join("tools/Isoplane.Cli/Isoplane.Cli.csproj"))
        .arg("--artifacts-path")
        .arg(destination.join("cli-artifacts"))
        .arg("-o")
        .arg(&packages)
        .arg("--disable-build-servers"));
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
    tools.join("isoplane")
}
