use isoplane_test_support::run_successfully;
use std::process::Command;

#[test]
fn the_installed_dotnet_tool_reports_its_package_version() {
    let cli = isoplane_fixtures::managed_cli();
    let installed = run_successfully(
        Command::new("dotnet")
            .args(["tool", "list", "--tool-path"])
            .arg(
                cli.parent()
                    .expect("the installed CLI has a tool directory"),
            ),
        "read the installed tool registry",
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
    let output = run_successfully(
        Command::new(cli).arg("--version"),
        "inspect the installed CLI's version",
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
