//! The canonical release build, consumed the way a customer does: a global tool
//! install, `isoplane setup`, the template, both execution modes, and the upgrade
//! to the next preview. Two releases are built from a snapshot of the working
//! tree, so the journey validates the sources as they are, not the last commit.
//! Every install runs inside a fresh Ubuntu 24.04 container carrying only the
//! prerequisites the release README names, with the releases mounted read-only.

use std::borrow::BorrowMut;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

struct Release {
    directory: PathBuf,
    version: String,
    revision: String,
}

struct Releases {
    _scratch: tempfile::TempDir,
    directory: PathBuf,
    image: String,
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
        "user.email=release-journey@wisp.invalid",
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
            .arg(root.join(".wisp-dev/preview-build"))
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

fn build_releases() -> Releases {
    let root = repo_root();
    let scratch = tempfile::tempdir().expect("scratch space for two releases");
    let commit = snapshot_working_tree(&root, &scratch.path().join("index"));
    let worktree = scratch.path().join("worktree");
    succeed(
        Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(&worktree)
            .arg(&commit)
            .current_dir(&root),
        "checking out the snapshot",
    );
    let output = scratch.path().join("releases");
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
                "user.email=release-journey@wisp.invalid",
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
    Releases {
        _scratch: scratch,
        directory: output,
        image,
        current,
        next,
    }
}

/// The apt packages the release README tells a customer to install, and nothing else.
fn documented_prerequisites(release: &Release) -> Vec<String> {
    let readme = std::fs::read_to_string(release.directory.join("README.md"))
        .expect("the release ships its README");
    let line = readme
        .lines()
        .find(|line| line.starts_with("The OS needs"))
        .expect("the release README names the OS prerequisites");
    let packages: Vec<String> = line
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect();
    assert!(
        !packages.is_empty(),
        "the README prerequisite sentence must name packages"
    );
    packages
}

/// A fresh Ubuntu 24.04 image with the documented prerequisites and an ordinary user; no .NET, no Isoplane.
fn customer_image(release: &Release) -> String {
    let dockerfile = format!(
        "FROM ubuntu:24.04\nRUN apt-get update && apt-get install -y --no-install-recommends {} && rm -rf /var/lib/apt/lists/*\nRUN useradd --create-home dev\nUSER dev\nWORKDIR /home/dev\n",
        documented_prerequisites(release).join(" ")
    );
    let tag = format!(
        "wisp-release-journey:{}",
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

/// A customer's machine: a container started from the fresh image with the releases mounted read-only.
struct Machine {
    name: String,
}

const RELEASES_MOUNT: &str = "/releases";

impl Machine {
    fn new(releases: &Releases) -> Self {
        let name = format!("wisp-release-journey-{}", uuid_like());
        succeed(
            Command::new("docker")
                .args([
                    "run",
                    "--detach",
                    "--name",
                    &name,
                    "--label",
                    "wisp.release-journey=1",
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
        Self { name }
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

    /// The documented environment once setup has installed the toolchain.
    fn installed(&self, script: &str) -> Command {
        self.shell(&format!(
            "export ISOPLANE_HOME=\"$HOME/.isoplane\"\nexport DOTNET_ROOT=\"$ISOPLANE_HOME/dotnet\"\nexport PATH=\"$DOTNET_ROOT:$HOME/.dotnet/tools:$PATH\"\n{script}"
        ))
    }

    /// The documented environment before setup: the release's own SDK bootstraps the tool.
    fn bootstrap(&self, release: &Release, script: &str) -> Command {
        self.shell(&format!(
            "preview_release={}\nexport DOTNET_ROOT=\"$preview_release/toolchain/dotnet\"\nexport PATH=\"$DOTNET_ROOT:$HOME/.dotnet/tools:$PATH\"\n{script}",
            Self::mounted(release)
        ))
    }

    fn install_cli(&self, release: &Release, verb: &str) {
        succeed(
            self.bootstrap(
                release,
                &format!(
                    "dotnet tool {verb} --global Isoplane.Cli --version {} --add-source \"$preview_release/packages\"",
                    release.version
                ),
            ),
            "installing the CLI as a global tool",
        );
    }

    fn install(&self, release: &Release, verb: &str) {
        self.install_cli(release, verb);
        let version = succeed(
            self.bootstrap(release, "isoplane --version"),
            "isoplane --version",
        );
        assert_eq!(
            version.trim(),
            format!("isoplane {}+{}", release.version, release.revision),
            "the installed CLI must report the release's version and revision"
        );
        succeed(
            self.bootstrap(release, "isoplane setup --source \"$preview_release\""),
            "isoplane setup",
        );
        succeed(
            self.installed("isoplane doctor"),
            "isoplane doctor after setup",
        );
    }

    fn install_template(&self, release: &Release) {
        let mounted = Self::mounted(release);
        succeed(
            self.installed(&format!(
                "dotnet new uninstall Wisp.Templates >/dev/null 2>&1 || true\ndotnet new install \"{mounted}/packages/Wisp.Templates.{}.nupkg\"",
                release.version
            )),
            "installing the release's template",
        );
    }

    fn create_project(&self, release: &Release, name: &str) -> String {
        let mounted = Self::mounted(release);
        let project = succeed(
            self.installed(&format!(
                "dotnet new isoplane-web --name {name} --output {name} >/dev/null\ncd {name}\ndotnet nuget add source \"{mounted}/packages\" --name isoplane-preview --configfile nuget.config >/dev/null\ncat {name}.csproj"
            )),
            "creating a project from the installed template",
        );
        assert!(
            project.contains(&format!("Sdk=\"Wisp.AspNetCore/{}\"", release.version)),
            "the generated project must pin the SDK version this release ships, not an older one:\n{project}"
        );
        name.to_owned()
    }

    fn build(&self, app: &str, configuration: &str) {
        let modules = succeed(
            self.installed(&format!(
                "cd {app}\ndotnet build -c {configuration} >build-{configuration}.log 2>&1 || {{ cat build-{configuration}.log; exit 1; }}\nls bin/{configuration}/net10.0/wasi-wasm/publish/*.wasm"
            )),
            &format!("building the {configuration} configuration with installed assets"),
        );
        assert!(
            modules.trim().ends_with(".wasm"),
            "the {configuration} build must publish a WebAssembly module from installed assets"
        );
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
fn a_clean_global_tool_install_and_setup_create_a_project_that_builds_in_both_modes() {
    let releases = releases();
    let machine = Machine::new(releases);
    machine.install(&releases.current, "install");
    let help = succeed(machine.installed("isoplane --help"), "isoplane --help");
    assert!(
        help.contains("isoplane login"),
        "the released CLI must offer the documented login command"
    );
    machine.install_template(&releases.current);
    let app = machine.create_project(&releases.current, "JourneyApp");
    for configuration in ["WispInterpretedDebug", "WispNativeAotRelease"] {
        machine.build(&app, configuration);
    }
}

#[test]
fn updating_the_global_tool_and_rerunning_setup_moves_to_the_next_preview_as_one_release() {
    let releases = releases();
    let machine = Machine::new(releases);
    machine.install(&releases.current, "install");
    machine.install_template(&releases.current);
    machine.install(&releases.next, "update");
    machine.install_template(&releases.next);
    let app = machine.create_project(&releases.next, "UpgradedApp");
    machine.build(&app, "WispInterpretedDebug");
}

#[test]
fn setup_refuses_the_release_of_another_revision_and_names_the_matching_cli_install() {
    let releases = releases();
    let machine = Machine::new(releases);
    machine.install_cli(&releases.next, "install");
    let error = fail(
        machine.bootstrap(
            &releases.next,
            &format!(
                "isoplane setup --source {}",
                Machine::mounted(&releases.current)
            ),
        ),
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
