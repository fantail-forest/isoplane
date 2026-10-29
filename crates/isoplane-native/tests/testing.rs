//! An ordinary test project consumes the local testing package and installed CLI.

use isoplane_test_support::run_successfully;
use std::process::Command;

#[test]
fn a_dotnet_test_project_runs_its_application_in_the_installed_sandbox() {
    let directory = tempfile::tempdir().unwrap();
    let app = isoplane_fixtures::handler_project(directory.path());
    let cli = isoplane_fixtures::managed_cli();
    let consumer = isoplane_fixtures::testing_consumer(directory.path());
    run_successfully(
        Command::new("dotnet")
            .args([
                "test",
                "--no-build",
                "--disable-build-servers",
                "--logger",
                "console;verbosity=normal",
            ])
            .env("TEST_APP", &app)
            .env("TEST_CLI", &cli)
            .env("ISOPLANE_CACHE_DIR", isoplane_fixtures::runtime_cache())
            .env(
                "ISOPLANE_GUEST_RUNTIME_FEED",
                isoplane_fixtures::guest_runtime_feed(),
            )
            .current_dir(consumer),
        "run the installed testing-package consumer with the .NET SDK",
    );
}
