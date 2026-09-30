use std::process::Command;

#[test]
fn the_installed_dotnet_tool_reports_its_package_version() {
    let directory = tempfile::tempdir().expect("create an isolated local tool feed");
    std::fs::write(
        directory.path().join("nuget.config"),
        r#"<configuration><packageSources><clear/></packageSources></configuration>"#,
    )
    .expect("configure the isolated local package feed");
    let source = std::env::var_os("ISOPLANE_TEST_NATIVE_PACKAGE_FEED")
        .expect("run task test:native to prepare the native package feed");
    let configured = Command::new("dotnet")
        .args(["nuget", "add", "source"])
        .arg(source)
        .args(["--name", "local", "--configfile"])
        .arg(directory.path().join("nuget.config"))
        .output()
        .expect("configure the prepared package feed");
    assert!(
        configured.status.success(),
        "configuring the prepared package feed failed: {}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let cli = isoplane_fixtures::managed_cli(directory.path(), directory.path());
    let installed = Command::new("dotnet")
        .args(["tool", "list", "--tool-path"])
        .arg(directory.path().join("tools"))
        .output()
        .expect("query the installed tool registry");
    assert!(
        installed.status.success(),
        "the installed tool registry must be readable"
    );
    let installed = String::from_utf8(installed.stdout).expect("the tool registry must emit UTF-8");
    let package_version = installed
        .lines()
        .find_map(|line| {
            let mut columns = line.split_whitespace();
            columns
                .next()?
                .eq_ignore_ascii_case("Isoplane.Cli")
                .then(|| columns.next())
                .flatten()
        })
        .expect("the local tool registry must contain Isoplane.Cli");
    let output = Command::new(cli)
        .arg("--version")
        .output()
        .expect("run the installed CLI");
    assert!(
        output.status.success(),
        "version inspection must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).expect("the CLI version must emit UTF-8");
    let version = output
        .trim()
        .strip_prefix("isoplane ")
        .expect("version output must identify the CLI");
    assert_eq!(
        version.split('+').next(),
        Some(package_version),
        "the CLI must report the version of its installed package"
    );
}
