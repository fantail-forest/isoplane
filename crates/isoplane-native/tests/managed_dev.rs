//! The installed managed CLI contains publication failures and cancels abandoned builds.

#[path = "common/dev.rs"]
pub mod common;

use common::Dev;
use std::path::{Path, PathBuf};

fn project(destination: &Path) -> (PathBuf, PathBuf) {
    (
        isoplane_fixtures::gated_project(destination),
        isoplane_fixtures::managed_cli(),
    )
}

fn source(app: &Path, response: &str, log: &str) {
    let source = format!(
        r#"
using Isoplane;

public static class Handler
{{
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request)
    {{
        System.Console.WriteLine("{log}");
        return Task.FromResult(Response.Text("{response}"));
    }}
}}
"#
    );
    std::fs::write(app.join("Handler.cs"), source).expect("customer application source");
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
    let mut dev = Dev::start_managed(&app, &cli);
    assert_eq!(
        body(&dev).await,
        "working publication",
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
    dev.wait_event("build_failed");
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
