//! Installs, uses and removes an explicitly prepared Linux release in a fresh customer container.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Release {
    directory: PathBuf,
    version: String,
    revision: String,
}

fn prepared_release() -> Release {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = std::env::var_os("ISOPLANE_TEST_RELEASE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            root.join(format!(
                ".build/releases/isoplane-linux-x64-{}",
                isoplane_fixtures::release_version()
            ))
        });
    let bytes = std::fs::read(directory.join("release.json")).unwrap_or_else(|error| {
        panic!("Prepare the release explicitly with scripts/build-preview.py, or set ISOPLANE_TEST_RELEASE to a prepared release directory: {}: {error}", directory.display())
    });
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).expect("release.json is JSON");
    assert_eq!(
        manifest["runtimeIdentifier"], "linux-x64",
        "the Linux installation journey needs a Linux release"
    );
    Release {
        directory: directory
            .canonicalize()
            .expect("the prepared release directory exists"),
        version: manifest["packageVersion"]
            .as_str()
            .expect("the release names its package version")
            .to_owned(),
        revision: manifest["sourceRevision"]
            .as_str()
            .expect("the release names its source revision")
            .to_owned(),
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

#[test]
fn a_prepared_release_installs_builds_applications_and_uninstalls() {
    let release = prepared_release();
    let image = customer_image(&release);
    let output = Command::new("docker")
        .args(["run", "--rm", "--init", "--volume"])
        .arg(format!("{}:/release:ro", release.directory.display()))
        .args(["--env", &format!("RELEASE_VERSION={}", release.version)])
        .args(["--env", &format!("RELEASE_REVISION={}", release.revision)])
        .arg(image)
        .args(["bash", "-euo", "pipefail", "-c", r#"
export PATH="$HOME/.dotnet/tools:$PATH"
dotnet tool install --global Isoplane.Cli --version "$RELEASE_VERSION" --add-source /release/packages
test "$(isoplane --version)" = "isoplane $RELEASE_VERSION+$RELEASE_REVISION"
isoplane setup --source /release --yes
isoplane doctor
dotnet new isoplane-web --name JourneyApp --output JourneyApp
cd JourneyApp
for configuration in Debug Release; do
    dotnet build -c "$configuration"
    isoplane bundle --configuration "$configuration" --output "$configuration.tar"
    tar -tf "$configuration.tar"
done
isoplane uninstall-sdk
test ! -e "$HOME/.isoplane/current.json"
test -f JourneyApp.csproj
if dotnet new list | grep -q isoplane-web; then
    echo 'Uninstall left the Isoplane template registered' >&2
    exit 1
fi
"#])
        .output()
        .expect("Docker is required for the prepared-release installation journey");
    assert!(
        output.status.success(),
        "the prepared release must install, build applications and uninstall:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8_lossy(&output.stdout);
    for entry in [
        "application.json",
        "runtime.json",
        "assemblies/JourneyApp.dll",
    ] {
        assert_eq!(
            output.lines().filter(|line| *line == entry).count(),
            2,
            "both application configurations must bundle {entry}"
        );
    }
}
