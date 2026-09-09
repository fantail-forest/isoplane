//! The installed managed CLI contains publication failures and cancels abandoned builds.

#[path = "common/dev.rs"]
pub mod common;

use common::{guest, manifest, Dev};
use std::path::{Path, PathBuf};

fn project(destination: &Path) -> (PathBuf, PathBuf) {
    let app = destination.join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::create_dir(destination.join("packages")).unwrap();
    manifest(&app, "");
    std::fs::write(app.join("fixture.wasm"), guest("working publication")).unwrap();
    std::fs::write(
        app.join("Lifecycle.csproj"),
        r#"<Project>
  <Target Name="Restore" />
  <Target Name="Publish">
    <Exec Condition="Exists('hold-publication')" Command="sh wait-build.sh" />
    <MakeDir Directories="$(PublishDir)" />
    <Copy Condition="!Exists('omit-publication')" SourceFiles="fixture.wasm" DestinationFolder="$(PublishDir)" />
  </Target>
</Project>"#,
    )
    .unwrap();
    std::fs::write(
        app.join("wait-build.sh"),
        "set -eu\nmkfifo .wisp/publication-gate\nprintf 'publication-is-held\\n'\nexec cat .wisp/publication-gate\n",
    )
    .unwrap();
    let packages = destination
        .join("packages")
        .to_string_lossy()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;");
    std::fs::write(
        app.join("nuget.config"),
        format!(
            "<configuration><packageSources><clear/><add key=\"local\" value=\"{packages}\"/></packageSources></configuration>"
        ),
    )
    .unwrap();
    let cli = wisp_fixtures::managed_cli(destination, Path::new(env!("CARGO_BIN_EXE_wisp")), &app);
    (app, cli)
}

async fn body(dev: &Dev) -> String {
    reqwest::get(&dev.url)
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .text()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_missing_publication_preserves_the_running_app_and_allows_repair() {
    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    let mut dev = Dev::start_managed(&app, &cli);
    assert_eq!(
        body(&dev).await,
        "working publication",
        "the installed CLI must serve its initial published module"
    );

    std::fs::write(app.join("omit-publication"), "").unwrap();
    dev.wait_for("rebuild failed; keeping the last working application");
    assert_eq!(
        body(&dev).await,
        "working publication",
        "a successful compiler exit without a module must not stop the working app"
    );

    let replacement = app.join(".wisp/repaired.wasm");
    std::fs::write(&replacement, guest("repaired publication")).unwrap();
    std::fs::rename(replacement, app.join("fixture.wasm")).unwrap();
    std::fs::remove_file(app.join("omit-publication")).unwrap();
    dev.wait_for("reloaded ");
    assert_eq!(
        body(&dev).await,
        "repaired publication",
        "fixing publication must reload without restarting the managed CLI"
    );
    dev.stop();
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_exit_cancels_a_publication_waiting_on_external_work() {
    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    let mut dev = Dev::start_managed(&app, &cli);
    std::fs::write(app.join("hold-publication"), "").unwrap();
    dev.wait_for("publication-is-held");

    // An unreadable module terminates the runtime while the publisher's FIFO stays held.
    std::fs::remove_file(app.join("app.wasm")).unwrap();
    std::fs::create_dir(app.join("app.wasm")).unwrap();
    dev.wait_for("cancelled publication because the runtime exited");
    assert!(
        app.join(".wisp/publication-gate").exists(),
        "runtime exit must cancel publication without requiring its external work to finish"
    );
    assert!(
        std::fs::read_dir(app.join(".wisp/publish"))
            .unwrap()
            .next()
            .is_none(),
        "cancelled publication must remove its staging directory before reporting completion"
    );
}
