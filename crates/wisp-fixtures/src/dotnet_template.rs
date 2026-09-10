use std::path::{Path, PathBuf};
use std::process::Command;

/// Installs the packaged ASP.NET template into an isolated hive and creates a consumer.
/// The returned project resolves WISP packages exclusively from its local feed.
pub fn aspnet_project(destination: &Path) -> PathBuf {
    let root = super::repo_root();
    let packages = destination.join("packages");
    for project in [
        "Wisp.Sdk.csproj",
        "AspNetCore/Wisp.AspNetCore.csproj",
        "Templates/Wisp.Templates.csproj",
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
        .arg(packages.join("Wisp.Templates.0.1.0.nupkg"))
        .arg("--debug:custom-hive")
        .arg(&hive));
    let app = destination.join("app");
    run(Command::new("dotnet")
        .args([
            "new",
            "isoplane-web",
            "--name",
            "Template-App",
            "--tenant",
            "template-owner",
        ])
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
    // A project with source mapping must map the source-built runtime pack's
    // ids to the feed that offers them, as a customer's would; the feed is
    // this checkout's, offered to the Mono profile through the environment
    // the dev harness sets.
    let feed = crate::guest_runtime_feed();
    let runtime_mapping = if feed.is_dir() {
        run(Command::new("dotnet")
            .args(["nuget", "add", "source"])
            .arg(&feed)
            .args(["--name", "wisp-guest-runtime", "--configfile"])
            .arg(&config));
        let patterns: String = crate::feed_package_ids()
            .iter()
            .map(|id| format!("<package pattern=\"{id}\" />"))
            .collect();
        format!("<packageSource key=\"wisp-guest-runtime\">{patterns}</packageSource>\n")
    } else {
        String::new()
    };
    let xml = std::fs::read_to_string(&config).expect("read generated package sources");
    let xml = xml.replace(
        "</configuration>",
        &format!(
            "<config><add key=\"globalPackagesFolder\" value=\"{}\" /></config>\n\
             <packageSourceMapping>\n\
             <packageSource key=\"template-packages\"><package pattern=\"Wisp.*\" /><package pattern=\"Isoplane.*\" /></packageSource>\n\
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

/// Packs and installs the managed CLI with a supplied native runtime into a local tool directory.
pub fn managed_cli(destination: &Path, runtime: &Path, app: &Path) -> PathBuf {
    let packages = destination.join("packages");
    run(Command::new("dotnet")
        .arg("pack")
        .arg(super::repo_root().join("tools/Isoplane.Cli/Isoplane.Cli.csproj"))
        .arg(format!("-p:WispRuntimePath={}", runtime.display()))
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
        .args([
            "tool",
            "install",
            "Isoplane.Cli",
            "--version",
            "0.1.0",
            "--tool-path",
        ])
        .arg(&tools)
        .arg("--configfile")
        .arg(app.join("nuget.config"))
        .current_dir(destination));
    tools.join("isoplane")
}
