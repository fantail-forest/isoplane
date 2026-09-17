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
    let cli = wisp_fixtures::managed_cli(destination, &app);
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
async fn guest_logs_reach_the_installed_cli_from_execution_threads() {
    let directory = tempfile::tempdir().expect("isolate the installed logging workflow");
    let (app, cli) = project(directory.path());
    std::fs::write(
        app.join("fixture.wasm"),
        common::guest_with_log("logged response", "managed-session-guest-log"),
    )
    .expect("publish a guest that logs during dispatch");
    let mut dev = Dev::start_managed(&app, &cli);
    assert_eq!(
        body(&dev).await,
        "logged response",
        "the logging guest must finish its request"
    );
    dev.wait_for("managed-session-guest-log");
    dev.stop();
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
    dev.wait_event("reloaded");
    assert_eq!(
        body(&dev).await,
        "repaired publication",
        "fixing publication must reload without restarting the managed CLI"
    );
    dev.stop();
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_cancels_a_publication_waiting_on_external_work() {
    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    let mut dev = Dev::start_managed(&app, &cli);
    std::fs::write(app.join("hold-publication"), "").unwrap();
    dev.wait_for("publication-is-held");

    dev.stop();
    assert!(
        app.join(".wisp/publication-gate").exists(),
        "shutdown must cancel publication without requiring its external work to finish"
    );
    assert!(
        std::fs::read_dir(app.join(".wisp/publish"))
            .unwrap()
            .next()
            .is_none(),
        "cancelled publication must remove its staging directory before reporting completion"
    );
}

#[tokio::test]
async fn a_keep_alive_connection_serves_the_new_publication_after_reload() {
    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;

    async fn request(sender: &mut hyper::client::conn::http1::SendRequest<Empty<Bytes>>) -> Bytes {
        let request = hyper::Request::builder()
            .uri("/")
            .header("host", "localhost")
            .body(Empty::new())
            .unwrap();
        sender
            .send_request(request)
            .await
            .unwrap()
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
    }

    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    let mut running = Dev::start_managed(&app, &cli);
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let address = running.url.strip_prefix("http://").unwrap();
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .unwrap();
        let connection = tokio::spawn(connection);
        assert_eq!(
            request(&mut sender).await,
            "working publication",
            "the initial connection must serve the original publication"
        );
        let pending = app.join(".wisp/replacement.wasm");
        std::fs::write(&pending, guest("replacement publication")).unwrap();
        std::fs::rename(pending, app.join("fixture.wasm")).unwrap();
        running.wait_event("reloaded");
        assert_eq!(
            request(&mut sender).await,
            "replacement publication",
            "the same TCP connection must serve the replacement publication after reload"
        );
        running.stop();
        connection.await.unwrap().unwrap();
    })
    .await
    .expect("the persistent HTTP connection must not hang across reload");
}

#[test]
fn a_second_managed_process_cannot_share_the_projects_state() {
    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    let mut running = Dev::start_managed(&app, &cli);
    let output = std::process::Command::new(&cli)
        .args(["dev", "--no-build", "--no-watch", "--project"])
        .arg(&app)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "a second local process must not share live persisted state"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("another local development session"),
        "the failed session must identify the project's existing owner: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    running.stop();
}

#[test]
fn the_cutover_supervisor_starts_an_installed_managed_worker() {
    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    std::fs::write(app.join("app.wasm"), guest("cutover worker")).unwrap();
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/local/cutover.py");
    let output = std::process::Command::new("python3")
        .args(["-c", r#"
import asyncio, importlib.util, sys, urllib.request
from types import SimpleNamespace
spec = importlib.util.spec_from_file_location("cutover", sys.argv[1])
cutover = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cutover)
async def run():
    supervisor = cutover.Cutover(SimpleNamespace(cli=sys.argv[2], output=sys.argv[4], startup_timeout=30))
    owner, url = await supervisor.worker("managed", sys.argv[3])
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            assert response.read() == b"cutover worker", "the managed worker must serve the real guest"
    finally:
        await owner.kill()
asyncio.run(run())
"#])
        .arg(script).arg(cli).arg(app).arg(directory.path())
        .output().unwrap();
    assert!(
        output.status.success(),
        "cutover must start and release an installed managed worker: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
