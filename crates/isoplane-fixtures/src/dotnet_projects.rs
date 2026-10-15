//! .NET projects built with the installed SDK, and the installed CLI, which
//! `task fixtures:projects` prepares in `ISOPLANE_TEST_DOTNET`. A test copies the project it
//! uses, because running or building a project writes into it.

use std::path::{Path, PathBuf};

/// Written last by the Task step, so a directory holding it was prepared completely.
const PREPARED_MARKER: &str = "prepared";

fn prepared() -> PathBuf {
    let path = std::env::var_os("ISOPLANE_TEST_DOTNET")
        .map(PathBuf::from)
        .expect("run task fixtures:projects to select the prepared .NET projects");
    assert!(
        path.join(PREPARED_MARKER).is_file(),
        "the prepared .NET projects are missing at {}; run task fixtures:projects",
        path.display()
    );
    path
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

/// A copy in `destination/app` of the handler application in
/// `crates/isoplane-native/tests/fixtures/handler`, with the library it references in
/// `destination/library`. `Handler.cs` lists the routes; any other path answers
/// "Hello from Isoplane!".
pub fn handler_project(destination: &Path) -> PathBuf {
    copy_tree(
        &prepared().join("fixtures/library"),
        &destination.join("library"),
    );
    copy_project(
        &prepared().join("fixtures/handler"),
        &destination.join("app"),
    )
}

/// A copy in `destination/app` of the handler application in
/// `crates/isoplane-native/tests/fixtures/gated`, which answers "working publication" and logs
/// `managed-session-guest-log`. Its build fails while the project holds a file named
/// `omit-publication`, and waits on `wait-build.sh` while it holds `hold-publication`.
pub fn gated_project(destination: &Path) -> PathBuf {
    copy_project(&prepared().join("fixtures/gated"), &destination.join("app"))
}

/// A copy in `destination/consumer` of the xunit project in `tests/fixtures/testing`,
/// built against the `Isoplane.Testing` package. It tests the application named by
/// `TEST_APP` with the CLI named by `TEST_CLI`.
pub fn testing_consumer(destination: &Path) -> PathBuf {
    copy_project(
        &prepared().join("fixtures/testing"),
        &destination.join("consumer"),
    )
}

/// A copy in `destination/app` of the packaged template `template`, generated under the name
/// `taskfiles/fixtures.yml` gives it and restored, but not built.
pub fn template_project(destination: &Path, template: &str) -> PathBuf {
    copy_project(
        &prepared().join("templates").join(template),
        &destination.join("app"),
    )
}

/// The `isoplane` command installed from the packaged CLI, shared by every test.
pub fn managed_cli() -> PathBuf {
    prepared()
        .join("tool")
        .join(format!("isoplane{}", std::env::consts::EXE_SUFFIX))
}

/// Copies a prepared project, keeping each file's modification time so that the build
/// sees its outputs as current.
fn copy_project(source: &Path, destination: &Path) -> PathBuf {
    copy_tree(source, destination);
    destination.to_owned()
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination)
        .unwrap_or_else(|e| panic!("create {}: {e}", destination.display()));
    for entry in
        std::fs::read_dir(source).unwrap_or_else(|e| panic!("read {}: {e}", source.display()))
    {
        let entry = entry.expect("list a prepared project");
        let target = destination.join(entry.file_name());
        let metadata = entry.metadata().expect("inspect a prepared file");
        if metadata.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target)
                .unwrap_or_else(|e| panic!("copy {}: {e}", entry.path().display()));
            std::fs::File::options()
                .write(true)
                .open(&target)
                .and_then(|file| file.set_modified(metadata.modified()?))
                .unwrap_or_else(|e| panic!("date {}: {e}", target.display()));
        }
    }
}
