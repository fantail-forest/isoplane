//! The installed managed CLI contains publication failures and cancels abandoned builds.

#[path = "common/dev.rs"]
pub mod common;

use common::Dev;
use std::path::{Path, PathBuf};

fn project(destination: &Path) -> (PathBuf, PathBuf) {
    let app = isoplane_fixtures::aspnet_project(destination);
    source(&app, "working publication", "");
    let project = app.join("Template-App.csproj");
    let text = std::fs::read_to_string(&project).expect("generated project");
    std::fs::write(
        &project,
        text.replace(
            "</Project>",
            r#"
  <Target Name="FixtureBuildGate" BeforeTargets="CoreCompile">
    <Error Condition="Exists('omit-publication')" Text="fixture package build refused" />
    <Exec Condition="Exists('hold-publication')" Command="sh wait-build.sh" />
  </Target>
</Project>"#,
        ),
    )
    .expect("fixture build gate");
    std::fs::write(app.join("wait-build.sh"),
        "set -eu\nmkfifo .isoplane/publication-gate\nprintf 'publication-is-held\\n'\nexec cat .isoplane/publication-gate\n")
        .expect("fixture external build gate");
    let cli = isoplane_fixtures::managed_cli(destination, &app);
    (app, cli)
}

fn source(app: &Path, response: &str, log: &str) {
    let source = format!(
        r#"
using Isoplane.AspNetCore;

var app = IsoplaneHosting.CreateBuilder(args).Build();
app.MapGet("/", () => {{ System.Console.WriteLine("{log}"); return "{response}"; }});
await app.RunAsync();
"#
    );
    std::fs::write(app.join("Program.cs"), source).expect("customer application source");
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
    source(&app, "logged response", "managed-session-guest-log");
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
async fn a_failed_package_build_preserves_the_running_app_and_allows_repair() {
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
        "a failed package build must not stop the working app"
    );

    source(&app, "repaired publication", "");
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
        app.join(".isoplane/publication-gate").exists(),
        "shutdown must cancel publication without requiring its external work to finish"
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
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            sender
                .send_request(request)
                .await
                .unwrap()
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
        })
        .await
        .expect("the persistent connection must return each response")
    }

    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    let mut running = Dev::start_managed(&app, &cli);
    let (mut sender, connection) =
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let address = running.url.strip_prefix("http://").unwrap();
            let stream = tokio::net::TcpStream::connect(address).await.unwrap();
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .unwrap()
        })
        .await
        .expect("the persistent HTTP connection must be established");
    let connection = tokio::spawn(connection);
    assert_eq!(
        request(&mut sender).await,
        "working publication",
        "the initial connection must serve the original publication"
    );
    source(&app, "replacement publication", "");
    running.wait_event("reloaded");
    assert_eq!(
        request(&mut sender).await,
        "replacement publication",
        "the same TCP connection must serve the replacement publication after reload"
    );
    running.stop();
    tokio::time::timeout(std::time::Duration::from_secs(60), connection)
        .await
        .expect("the connection must close after development shutdown")
        .unwrap()
        .unwrap();
}

#[test]
fn a_second_managed_process_cannot_share_the_projects_state() {
    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    let mut running = Dev::start_managed(&app, &cli);
    let output = std::process::Command::new(&cli)
        .args(["dev", "--no-watch", "--project"])
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
#[cfg(target_os = "linux")] // The local cutover supervisor requires Linux process sessions and /proc.
fn the_cutover_supervisor_starts_an_installed_managed_worker() {
    let directory = tempfile::tempdir().unwrap();
    let (app, cli) = project(directory.path());
    source(&app, "cutover worker", "");
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/local/cutover.py");
    let python = std::env::var_os("ISOPLANE_TEST_PYTHON")
        .expect("run tests through Task to select the configured Python interpreter");
    let output = std::process::Command::new(python)
        .args(["-c", r#"
import asyncio, importlib.util, sys, urllib.request
from types import SimpleNamespace
spec = importlib.util.spec_from_file_location("cutover", sys.argv[1])
cutover = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cutover)
async def run():
    supervisor = cutover.Cutover(SimpleNamespace(cli=sys.argv[2], output=sys.argv[4], startup_timeout=600, no_build=False, configuration="Debug"))
    owner, url = await supervisor.worker("managed", sys.argv[3])
    try:
        with urllib.request.urlopen(url, timeout=30) as response:
            assert response.read() == b"cutover worker", "the managed worker must serve the real guest"
    finally:
        await owner.kill()
asyncio.run(run())
"#])
        .arg(script).arg(cli).arg(app).arg(directory.path())
        .env("ISOPLANE_CACHE_DIR", directory.path().join("cache"))
        .output().unwrap();
    assert!(
        output.status.success(),
        "cutover must start and release an installed managed worker: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
