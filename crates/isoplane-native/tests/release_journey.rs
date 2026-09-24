//! The canonical release build, consumed the way a customer does: the bootstrap
//! .NET SDK from Ubuntu's packages, a global tool install, `isoplane setup`
//! downloading the release from an origin, the registered template, both
//! execution modes, the upgrade to the next preview, and `isoplane uninstall-sdk`.
//! Two releases are built from a snapshot of the working tree, so the journey
//! validates the sources as they are, not the last commit. Every install runs
//! inside a fresh Ubuntu 24.04 container carrying only the bootstrap SDK the
//! release README names, with the releases served over HTTP from this process
//! and mounted read-only for the offline `--source` path.

use std::borrow::BorrowMut;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

struct Release {
    directory: PathBuf,
    version: String,
    revision: String,
}

struct Releases {
    directory: PathBuf,
    image: String,
    origin: Origin,
    current: Release,
    next: Release,
}

/// Built once per test binary; a failed build is reported by every test rather than retried.
static RELEASES: OnceLock<Result<Releases, String>> = OnceLock::new();

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root exists")
}

fn releases() -> &'static Releases {
    match RELEASES.get_or_init(|| {
        std::panic::catch_unwind(build_releases).map_err(|payload| {
            payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_else(|| "the release build panicked".to_owned())
        })
    }) {
        Ok(releases) => releases,
        Err(error) => panic!("the releases this journey needs could not be built: {error}"),
    }
}

/// Commits a snapshot of the working tree (tracked and untracked, ignored files excluded)
/// as a dangling commit on top of HEAD, leaving the index and HEAD untouched.
fn snapshot_working_tree(root: &Path, index: &Path) -> String {
    let git = |args: &[&str]| -> String {
        let output = Command::new("git")
            .args(args)
            .env("GIT_INDEX_FILE", index)
            .current_dir(root)
            .output()
            .expect("git is required to snapshot the working tree");
        assert!(
            output.status.success(),
            "git {} must succeed to snapshot the working tree:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let head = git(&["rev-parse", "HEAD"]);
    git(&["read-tree", &head]);
    git(&["add", "-A"]);
    let tree = git(&["write-tree"]);
    git(&[
        "-c",
        "user.name=release journey",
        "-c",
        "user.email=release-journey@isoplane.invalid",
        "commit-tree",
        &tree,
        "-p",
        &head,
        "-m",
        "release journey snapshot of the working tree",
    ])
}

fn succeed(mut command: impl BorrowMut<Command>, purpose: &str) -> String {
    let command = command.borrow_mut();
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{purpose}: could not start {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{purpose} must succeed ({command:?}):\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn fail(mut command: impl BorrowMut<Command>, purpose: &str) -> String {
    let command = command.borrow_mut();
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{purpose}: could not start {command:?}: {error}"));
    assert!(
        !output.status.success(),
        "{purpose} must be refused ({command:?}):\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn build_release(worktree: &Path, root: &Path, output: &Path) -> Release {
    succeed(
        Command::new("python3")
            .arg(worktree.join("scripts/build-preview.py"))
            .arg("--cache")
            .arg(root.join(".isoplane-dev/preview-build"))
            .arg("--output")
            .arg(output)
            .current_dir(worktree),
        "the canonical release build",
    );
    let version = succeed(
        Command::new("python3")
            .arg(worktree.join("scripts/release-version.py"))
            .arg("show")
            .current_dir(worktree),
        "reading the declared release version",
    )
    .trim()
    .to_owned();
    let directory = output.join(format!("isoplane-linux-x64-{version}"));
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.join("release.json")).expect("the build writes release.json"),
    )
    .expect("release.json is JSON");
    assert_eq!(
        manifest["packageVersion"], version,
        "the release manifest must carry the declared version"
    );
    assert!(
        directory
            .join(manifest["toolchain"]["archive"].as_str().unwrap())
            .is_file(),
        "the release must ship the toolchain archive its manifest names"
    );
    assert!(
        output
            .join(format!("isoplane-linux-x64-{version}.tgz"))
            .is_file(),
        "the build must leave the release archive beside the release directory"
    );
    Release {
        directory,
        version,
        revision: manifest["sourceRevision"]
            .as_str()
            .expect("release.json records the source revision")
            .to_owned(),
    }
}

/// The scratch tree for two releases lives under `.isoplane-dev` and is rebuilt on every run, so a finished
/// run leaves one copy behind rather than one per run in a temporary directory nobody removes.
fn scratch_tree(root: &Path) -> PathBuf {
    let scratch = root.join(".isoplane-dev/release-journey");
    if scratch.exists() {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(scratch.join("worktree"))
            .current_dir(root)
            .output();
        std::fs::remove_dir_all(&scratch).expect("remove the previous run's scratch tree");
    }
    std::fs::create_dir_all(&scratch).expect("create the scratch tree");
    scratch
}

fn build_releases() -> Releases {
    let root = repo_root();
    let scratch = scratch_tree(&root);
    let commit = snapshot_working_tree(&root, &scratch.join("index"));
    let worktree = scratch.join("worktree");
    succeed(
        Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&worktree)
            .arg(&commit)
            .current_dir(&root),
        "checking out the snapshot",
    );
    let output = scratch.join("releases");
    let current = build_release(&worktree, &root, &output);
    let next_version = format!("{}.upgrade.1", current.version);
    succeed(
        Command::new("python3")
            .arg(worktree.join("scripts/release-version.py"))
            .arg("set")
            .arg(&next_version)
            .current_dir(&worktree),
        "moving the declared version to the next preview",
    );
    succeed(
        Command::new("git")
            .args([
                "-c",
                "user.name=release journey",
                "-c",
                "user.email=release-journey@isoplane.invalid",
                "commit",
                "-qam",
                "release journey: next preview",
            ])
            .current_dir(&worktree),
        "committing the next preview version",
    );
    let next = build_release(&worktree, &root, &output);
    assert_eq!(
        next.version, next_version,
        "the next release must carry the moved version"
    );
    assert_ne!(
        current.revision, next.revision,
        "two previews come from two revisions"
    );
    succeed(
        Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&worktree)
            .current_dir(&root),
        "removing the snapshot checkout",
    );
    let image = customer_image(&current);
    let origin = Origin::serve(output.clone());
    Releases {
        directory: output,
        image,
        origin,
        current,
        next,
    }
}

/// The one apt command the release README tells a customer to run before installing the tool.
fn documented_bootstrap_packages(release: &Release) -> Vec<String> {
    let readme = std::fs::read_to_string(release.directory.join("README.md"))
        .expect("the release ships its README");
    let line = readme
        .lines()
        .find(|line| line.starts_with("sudo apt-get install -y "))
        .expect("the release README gives the bootstrap apt command");
    let packages: Vec<String> = line["sudo apt-get install -y ".len()..]
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    assert!(
        !packages.is_empty(),
        "the README bootstrap command must name packages"
    );
    packages
}

/// A fresh Ubuntu 24.04 image with the documented bootstrap SDK and an ordinary user; nothing of Isoplane.
fn customer_image(release: &Release) -> String {
    let dockerfile = format!(
        "FROM ubuntu:24.04\nRUN apt-get update && apt-get install -y --no-install-recommends {} && rm -rf /var/lib/apt/lists/*\nRUN useradd --create-home dev\nUSER dev\nWORKDIR /home/dev\n",
        documented_bootstrap_packages(release).join(" ")
    );
    let tag = format!(
        "isoplane-release-journey:{}",
        blake3::hash(dockerfile.as_bytes()).to_hex()
    );
    let mut build = Command::new("docker");
    build
        .args(["build", "--quiet", "--tag", &tag, "-"])
        .stdin(std::process::Stdio::piped());
    let mut child = build
        .spawn()
        .expect("docker is required to provision a fresh customer machine");
    std::io::Write::write_all(
        child.stdin.as_mut().expect("piped stdin"),
        dockerfile.as_bytes(),
    )
    .expect("hand docker the image definition");
    let output = child.wait_with_output().expect("docker build must finish");
    assert!(
        output.status.success(),
        "the customer image must build:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    tag
}

/// The release origin: this process serving the releases directory over HTTP to the containers.
/// Under `/cut/` the toolchain archive is served truncated, which is how a download is interrupted on demand.
struct Origin {
    port: u16,
    _runtime: tokio::runtime::Runtime,
}

const HOST_ALIAS: &str = "host.docker.internal";

impl Origin {
    fn serve(root: PathBuf) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the release origin");
        let listener = runtime.block_on(async {
            tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], 0)))
                .await
                .expect("bind the release origin")
        });
        let port = listener.local_addr().unwrap().port();
        runtime.spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let root = root.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |request| {
                        let root = root.clone();
                        async move { Ok::<_, std::convert::Infallible>(respond(&root, request)) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self {
            port,
            _runtime: runtime,
        }
    }

    fn url(&self, prefix: &str) -> String {
        format!("http://{HOST_ALIAS}:{}/{prefix}", self.port)
    }
}

/// A response body served in chunks with no size hint, so a cut download can promise the whole file
/// in its headers and still end early, the way a dropped connection does.
struct Chunks {
    data: Vec<u8>,
    offset: usize,
}

impl hyper::body::Body for Chunks {
    type Data = hyper::body::Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        if self.offset >= self.data.len() {
            return std::task::Poll::Ready(None);
        }
        let end = (self.offset + (1 << 20)).min(self.data.len());
        let chunk = hyper::body::Bytes::copy_from_slice(&self.data[self.offset..end]);
        self.offset = end;
        std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(chunk))))
    }
}

fn respond(root: &Path, request: hyper::Request<hyper::body::Incoming>) -> hyper::Response<Chunks> {
    let path = request.uri().path().trim_start_matches('/');
    let (cut, relative) = match path.strip_prefix("cut/") {
        Some(rest) => (true, rest),
        None => (false, path),
    };
    let escapes = relative
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..");
    let file = root.join(relative);
    if escapes || !file.is_file() {
        return hyper::Response::builder()
            .status(404)
            .body(Chunks {
                data: Vec::new(),
                offset: 0,
            })
            .unwrap();
    }
    let content = std::fs::read(&file).expect("read a served release file");
    let length = content.len();
    let served = if cut && relative.ends_with("toolchain.tgz") {
        content[..length / 2].to_vec()
    } else {
        content
    };
    hyper::Response::builder()
        .status(200)
        .header("content-length", length)
        .body(Chunks {
            data: served,
            offset: 0,
        })
        .unwrap()
}

/// A customer's machine: a container started from the fresh image with the releases mounted read-only
/// and the origin reachable by name.
struct Machine {
    name: String,
}

const RELEASES_MOUNT: &str = "/releases";

impl Machine {
    fn new(releases: &Releases) -> Self {
        let name = format!("isoplane-release-journey-{}", uuid_like());
        succeed(
            Command::new("docker")
                .args([
                    "run",
                    "--detach",
                    "--name",
                    &name,
                    "--label",
                    "isoplane.release-journey=1",
                    "--add-host",
                    &format!("{HOST_ALIAS}:host-gateway"),
                ])
                .arg("--volume")
                .arg(format!(
                    "{}:{RELEASES_MOUNT}:ro",
                    releases.directory.display()
                ))
                .arg(&releases.image)
                .args(["sleep", "infinity"]),
            "starting a fresh customer machine",
        );
        let machine = Self { name };
        succeed(
            machine.shell(
                "printf '%s\\n' 'export PATH=\"$HOME/.dotnet/tools:$PATH\"' >> \"$HOME/.profile\"",
            ),
            "configuring the standard global-tool PATH prerequisite",
        );
        machine
    }

    fn mounted(release: &Release) -> String {
        format!(
            "{RELEASES_MOUNT}/{}",
            release.directory.file_name().unwrap().to_str().unwrap()
        )
    }

    fn shell(&self, script: &str) -> Command {
        let mut command = Command::new("docker");
        command
            .args([
                "exec",
                "--user",
                "dev",
                "--workdir",
                "/home/dev",
                &self.name,
            ])
            .args(["bash", "-eo", "pipefail", "-c", script]);
        command
    }

    /// A new terminal: a login shell with no environment hand-written by the test.
    fn new_shell(&self, script: &str) -> Command {
        let mut command = Command::new("docker");
        command
            .args([
                "exec",
                "--user",
                "dev",
                "--workdir",
                "/home/dev",
                &self.name,
            ])
            .args(["bash", "-leo", "pipefail", "-c", script]);
        command
    }

    /// The environment the README asks for before setup: the .NET tools directory on PATH.
    fn bootstrap(&self, script: &str) -> Command {
        self.shell(&format!(
            "export PATH=\"$HOME/.dotnet/tools:$PATH\"\n{script}"
        ))
    }

    fn install_cli(&self, release: &Release, verb: &str) {
        succeed(
            self.bootstrap(&format!(
                "dotnet tool {verb} --global Isoplane.Cli --version {} --add-source \"{}/packages\"",
                release.version,
                Self::mounted(release)
            )),
            "installing the CLI as a global tool with the bootstrap SDK",
        );
        let version = succeed(self.bootstrap("isoplane --version"), "isoplane --version");
        assert_eq!(
            version.trim(),
            format!("isoplane {}+{}", release.version, release.revision),
            "the installed CLI must report the release's version and revision"
        );
    }

    /// `isoplane setup` downloading from the origin, as the README documents.
    fn setup(&self, origin: &str) -> String {
        succeed(
            self.bootstrap(&format!(
                "ISOPLANE_RELEASE_ORIGIN={origin} isoplane setup --yes 2>setup.log || {{ cat setup.log; exit 1; }}"
            )),
            "isoplane setup from the release origin",
        )
    }

    fn install(&self, releases: &Releases, release: &Release, verb: &str) -> String {
        self.install_cli(release, verb);
        let report = self.setup(&releases.origin.url(""));
        assert!(
            report.contains(&format!("Installed Isoplane {}", release.version)),
            "setup must report the installed release:\n{report}"
        );
        succeed(
            self.new_shell("isoplane doctor"),
            "isoplane doctor in a new shell after setup",
        );
        report
    }

    fn create_project(&self, release: &Release, name: &str) -> String {
        let project = succeed(
            self.new_shell(&format!(
                "dotnet new isoplane-web --name {name} --output {name} >/dev/null\ncat {name}/{name}.csproj"
            )),
            "creating a project from the registered template in a new shell",
        );
        assert!(
            project.contains(&format!("Sdk=\"Isoplane.AspNetCore/{}\"", release.version)),
            "the generated project must pin the SDK version this release ships, not an older one:\n{project}"
        );
        name.to_owned()
    }

    fn build(&self, app: &str, configuration: &str) {
        let modules = succeed(
            self.new_shell(&format!(
                "cd {app}\ndotnet build -c {configuration} >build-{configuration}.log 2>&1 || {{ cat build-{configuration}.log; exit 1; }}\nls bin/{configuration}/net10.0/wasi-wasm/publish/*.wasm"
            )),
            &format!("building the {configuration} configuration with installed assets"),
        );
        assert!(
            modules.trim().ends_with(".wasm"),
            "the {configuration} build must publish a WebAssembly module from installed assets"
        );
    }

    fn templates(&self) -> String {
        succeed(
            self.new_shell("dotnet new list"),
            "dotnet new list in a new shell",
        )
    }

    /// Every path under the home directory that is not .NET's own state or a customer project.
    fn home_inventory(&self) -> Vec<String> {
        let listing = succeed(
            self.shell("find \"$HOME\" -mindepth 1 \\( -path \"$HOME/.dotnet\" -o -path \"$HOME/.nuget\" -o -path \"$HOME/.templateengine\" -o -path \"$HOME/.local\" -o -path \"$HOME/.cache\" -o -path \"$HOME/.config\" -o -name '*App' -o -name '*.log' \\) -prune -o -print | sort"),
            "listing the home directory",
        );
        listing.lines().map(str::to_owned).collect()
    }

    fn uninstall_sdk(&self) -> String {
        succeed(
            self.new_shell(
                "isoplane uninstall-sdk 2>uninstall.log || { cat uninstall.log; exit 1; }",
            ),
            "isoplane uninstall-sdk",
        )
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let removed = Command::new("docker")
            .args(["rm", "--force", &self.name])
            .output()
            .expect("docker removes the customer machine");
        assert!(
            removed.status.success(),
            "the customer machine must be removed: {}",
            String::from_utf8_lossy(&removed.stderr)
        );
    }
}

fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    format!("{nanos:x}-{:x}", std::process::id())
}

#[test]
fn a_global_tool_install_and_setup_from_the_origin_create_a_project_that_builds_in_both_modes() {
    let releases = releases();
    let machine = Machine::new(releases);
    let sdk_before = succeed(
        machine.new_shell("command -v dotnet"),
        "finding the existing SDK",
    );
    succeed(
        machine.new_shell("dotnet new console --name UnrelatedApp && dotnet build UnrelatedApp"),
        "building an unrelated project before setup",
    );
    succeed(
        machine.new_shell("dotnet workload install wasi-experimental --skip-manifest-update"),
        "installing a Microsoft workload before Isoplane",
    );
    machine.install(releases, &releases.current, "install");
    succeed(
        machine.new_shell("dotnet workload repair"),
        "repairing Microsoft and Isoplane workloads through the standard SDK command",
    );
    succeed(
        machine.new_shell("dotnet build UnrelatedApp"),
        "building the unrelated project with Isoplane installed",
    );
    let help = succeed(machine.new_shell("isoplane --help"), "isoplane --help");
    assert!(
        help.contains("isoplane login") && help.contains("isoplane uninstall-sdk"),
        "the released CLI must offer the documented login and uninstall-sdk commands"
    );
    let sdk = succeed(
        machine.new_shell("command -v dotnet"),
        "which dotnet a new shell runs",
    );
    assert_eq!(
        sdk.trim(),
        sdk_before.trim(),
        "setup must preserve SDK selection in a new shell"
    );
    assert!(
        machine.templates().contains("isoplane-web"),
        "dotnet new list must discover the registered template after setup"
    );
    let app = machine.create_project(&releases.current, "JourneyApp");
    for configuration in ["IsoplaneInterpretedDebug", "IsoplaneNativeAotRelease"] {
        machine.build(&app, configuration);
    }
    machine.uninstall_sdk();
    let workloads = succeed(
        machine.new_shell("dotnet workload list"),
        "listing workloads after Isoplane removal",
    );
    assert!(
        workloads.contains("wasi-experimental"),
        "removing Isoplane must retain the Microsoft workload"
    );
    succeed(
        machine.new_shell("dotnet build UnrelatedApp"),
        "building the unrelated project after removal",
    );
}

#[test]
fn repeated_setup_changes_nothing_and_uninstall_sdk_removes_exactly_what_setup_recorded() {
    let releases = releases();
    let machine = Machine::new(releases);
    machine.install_cli(&releases.current, "install");
    let before = machine.home_inventory();
    let profile_before = succeed(
        machine.shell("cat \"$HOME/.profile\""),
        "reading the original shell profile",
    );
    machine.setup(&releases.origin.url(""));
    let manifest = succeed(
        machine.shell("cat \"$HOME/.isoplane/installation.json\""),
        "reading the installation manifest",
    );
    let report = machine.setup(&releases.origin.url(""));
    assert!(
        report.contains(&format!("Installed Isoplane {}", releases.current.version)),
        "a repeated setup must succeed:\n{report}"
    );
    assert_eq!(
        succeed(
            machine.shell("cat \"$HOME/.isoplane/installation.json\""),
            "re-reading the installation manifest"
        ),
        manifest,
        "a repeated setup must record nothing new"
    );
    assert_eq!(
        succeed(
            machine.shell("cat \"$HOME/.profile\""),
            "reading the shell profile after repeated setup"
        ),
        profile_before,
        "setup must not alter the user's shell configuration"
    );
    let app = machine.create_project(&releases.current, "RemovalApp");
    machine.build(&app, "IsoplaneInterpretedDebug");

    let selected = succeed(
        machine.shell("cat \"$HOME/.isoplane/current.json\""),
        "locating the installed toolchain for the repair check",
    );
    let selected: String =
        serde_json::from_str(&selected).expect("the selected toolchain is a JSON path");
    succeed(
        machine.shell(&format!("printf broken > '{selected}/bin/wasm-tools'")),
        "damaging an owned tool to exercise setup repair",
    );
    fail(
        machine.new_shell("isoplane doctor"),
        "doctor detecting the damaged tool",
    );
    machine.setup(&releases.origin.url(""));
    succeed(
        machine.new_shell("isoplane doctor"),
        "doctor after setup repairs its workload",
    );

    let removal = machine.uninstall_sdk();
    assert!(
        removal.contains("Removed the Isoplane SDK"),
        "removal must report completion:\n{removal}"
    );
    let after = machine.home_inventory();
    // Keep the coordination file: unlinking it can let concurrent commands lock different inodes.
    let leftovers: Vec<&String> = after
        .iter()
        .filter(|path| {
            !before.contains(path)
                && !matches!(
                    path.as_str(),
                    "/home/dev/.isoplane" | "/home/dev/.isoplane/setup.lock"
                )
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "uninstall-sdk must leave no installed SDK resources:\n{leftovers:?}"
    );
    assert!(
        !machine.templates().contains("isoplane-web"),
        "the template registration must be gone after uninstall-sdk"
    );
    let sources = succeed(
        machine.new_shell("dotnet nuget list source"),
        "listing package sources after removal",
    );
    assert!(
        !sources.contains("isoplane"),
        "the package source registration must be gone after uninstall-sdk:\n{sources}"
    );
    succeed(
        machine.shell(&format!(
            "test -f {app}/{app}.csproj && test -f {app}/Application.cs"
        )),
        "the customer's project surviving removal",
    );
    let repeated = fail(
        machine.new_shell("isoplane uninstall-sdk"),
        "a second uninstall-sdk with nothing recorded",
    );
    assert!(
        repeated.contains("no installation manifest"),
        "a repeated removal must say nothing is recorded rather than guess:\n{repeated}"
    );
    machine.setup(&releases.origin.url(""));
    assert!(
        machine.templates().contains("isoplane-web"),
        "setup after removal must register the template again"
    );
    succeed(
        machine.new_shell("isoplane doctor"),
        "doctor after reinstalling the SDK",
    );
}

#[test]
fn updating_the_global_tool_and_rerunning_setup_moves_to_the_next_preview_as_one_release() {
    let releases = releases();
    let machine = Machine::new(releases);
    machine.install(releases, &releases.current, "install");
    let first = succeed(
        machine.shell("ls \"$HOME/.isoplane/toolchains\""),
        "listing installed toolchains",
    );
    machine.install(releases, &releases.next, "update");
    let toolchains = succeed(
        machine.shell("ls \"$HOME/.isoplane/toolchains\""),
        "listing installed toolchains after the upgrade",
    );
    assert!(
        toolchains.lines().count() == 1 && toolchains != first,
        "the upgrade must replace the toolchain rather than accumulate them:\n{toolchains}"
    );
    let app = machine.create_project(&releases.next, "UpgradedApp");
    machine.build(&app, "IsoplaneInterpretedDebug");
    machine.uninstall_sdk();
    assert!(
        !machine.templates().contains("isoplane-web"),
        "uninstall-sdk after an upgrade must remove the upgraded template registration"
    );
}

#[test]
fn setup_refuses_the_release_of_another_revision_and_names_the_matching_cli_install() {
    let releases = releases();
    let machine = Machine::new(releases);
    machine.install_cli(&releases.next, "install");
    let error = fail(
        machine.bootstrap(&format!(
            "isoplane setup --source {}",
            Machine::mounted(&releases.current)
        )),
        "installing a toolchain from a release the CLI was not built from",
    );
    assert!(
        error.contains(&format!(
            "dotnet tool update --global Isoplane.Cli --version {}",
            releases.current.version
        )),
        "the refusal must name the command that installs the matching CLI:\n{error}"
    );
    succeed(
        machine.shell("test ! -e \"$HOME/.isoplane/current.json\""),
        "a refused release leaving no toolchain selected",
    );
}

#[test]
fn an_interrupted_download_is_not_a_successful_setup_and_a_retry_completes_it() {
    let releases = releases();
    let machine = Machine::new(releases);
    machine.install_cli(&releases.current, "install");
    let error = fail(
        machine.bootstrap(&format!(
            "ISOPLANE_RELEASE_ORIGIN={} isoplane setup --yes",
            releases.origin.url("cut/")
        )),
        "setup with the toolchain download cut short",
    );
    assert!(
        error.contains("interrupted") && !error.contains("Installed Isoplane"),
        "an interrupted download must be reported as such and never as success:\n{error}"
    );
    succeed(
        machine.shell("test ! -e \"$HOME/.isoplane/current.json\" && test -f \"$HOME/.isoplane/installation.json\" && grep -q downloads \"$HOME/.isoplane/installation.json\""),
        "an interrupted setup leaving no toolchain selected but a manifest describing what it left",
    );
    assert!(
        !machine.templates().contains("isoplane-web"),
        "no template may be registered by an incomplete setup"
    );
    succeed(
        machine.shell("cp \"$HOME/.isoplane/installation.json\" manifest.json && echo '{ damaged' > \"$HOME/.isoplane/installation.json\""),
        "damaging the manifest",
    );
    let refusal = fail(
        machine.new_shell("isoplane uninstall-sdk"),
        "uninstall-sdk with a damaged manifest",
    );
    assert!(
        refusal.contains("installation.json") && refusal.contains("Nothing was removed"),
        "a damaged manifest must produce a diagnostic, not destructive cleanup:\n{refusal}"
    );
    succeed(
        machine.shell("test -d \"$HOME/.isoplane/packages\" && mv manifest.json \"$HOME/.isoplane/installation.json\""),
        "the partial installation surviving the refusal, then restoring the manifest",
    );
    machine.setup(&releases.origin.url(""));
    succeed(
        machine.new_shell("isoplane doctor && test ! -e \"$HOME/.isoplane/downloads\""),
        "the retried setup completing and discarding the download",
    );
    let selected = succeed(
        machine.shell("cat \"$HOME/.isoplane/current.json\""),
        "locating the installed toolchain for the repair check",
    );
    let selected: String =
        serde_json::from_str(&selected).expect("the selected toolchain is a JSON path");
    succeed(
        machine.shell(&format!("printf broken > '{selected}/bin/wasm-tools'")),
        "damaging an owned tool to exercise setup repair",
    );
    fail(
        machine.new_shell("isoplane doctor"),
        "doctor detecting the damaged tool",
    );
    machine.setup(&releases.origin.url(""));
    succeed(
        machine.new_shell("isoplane doctor"),
        "doctor after setup repairs its workload",
    );

    let removal = machine.uninstall_sdk();
    assert!(
        removal.contains("Removed the Isoplane SDK"),
        "removal after the recovered setup must complete:\n{removal}"
    );
}
