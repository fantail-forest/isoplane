//! Mono's pinned linker adaptation rejects changed inputs before invoking clang.

use std::path::Path;
use std::process::{Command, Output};

fn run_task(directory: &Path, project: &str, target: &str) -> Output {
    std::fs::write(
        directory.join("global.json"),
        include_str!("../../../global.json"),
    )
    .expect("task tests must use the repository's pinned .NET SDK");
    Command::new("dotnet")
        .args(["msbuild", project, target, "-nologo", "-nodeReuse:false"])
        .current_dir(directory)
        .output()
        .expect("the pinned .NET SDK must execute Mono's actual MSBuild task")
}

fn rewrite(directory: &Path, flags: &str) -> Output {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../guest-sdks/dotnet/Build");
    std::fs::write(directory.join("input.rsp"), flags).unwrap();
    let project = format!(
        r#"<Project>
  <UsingTask TaskName="WispMonoRewriteLink" TaskFactory="RoslynCodeTaskFactory" AssemblyFile="$(MSBuildToolsPath)/Microsoft.Build.Tasks.Core.dll">
    <Task><Code Type="Class" Language="cs" Source="{}/MonoRewriteLink.cs" /></Task>
  </UsingTask>
  <Target Name="Rewrite">
    <WispMonoRewriteLink Input="input.rsp" Output="output.rsp" ProjectDirectory="$(MSBuildProjectDirectory)" />
  </Target>
</Project>"#,
        sdk.display()
    );
    std::fs::write(directory.join("link.proj"), project).unwrap();
    run_task(directory, "link.proj", "-t:Rewrite")
}

#[test]
fn mono_linking_preserves_inputs_and_expands_the_runtime_response() {
    let directory = tempfile::tempdir().unwrap();
    let nested = "-Wl,-lwasi-emulated-pthread,--initial-memory=52428800\n";
    std::fs::write(directory.path().join("runtime.rsp"), nested).unwrap();
    let input = "-Wl,--no-entry,-lwasi-emulated-pthread\n\"@runtime.rsp\"\n";
    let result = rewrite(directory.path(), input);
    assert!(
        result.status.success(),
        "the pinned two-source link shape must succeed: {}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("input.rsp")).unwrap(),
        input,
        "the upstream link response must remain immutable"
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("runtime.rsp")).unwrap(),
        nested,
        "the runtime pack response must remain immutable"
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("output.rsp")).unwrap(),
        "-Wl,--no-entry\n-Wl,--initial-memory=52428800\n\n",
        "only the unsupported pthread archive flags may be removed"
    );
}

#[test]
fn mono_linking_refuses_changed_upstream_flag_counts() {
    for flags in [
        "-Wl,--no-entry",
        "-Wl,-lwasi-emulated-pthread",
        "-Wl,-lwasi-emulated-pthread,-lwasi-emulated-pthread,-lwasi-emulated-pthread",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let result = rewrite(directory.path(), flags);
        assert!(
            !result.status.success(),
            "changed upstream flags must fail before native linking: {flags}"
        );
        assert!(
            !directory.path().join("output.rsp").exists(),
            "a rejected response must not publish a rewritten output"
        );
    }
}

fn check_toolchain(directory: &Path, upstream: &str) -> Output {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../guest-sdks/dotnet/Build/MonoToolchainGuard.cs");
    let project = format!(
        r#"<Project>
  <UsingTask TaskName="WispMonoToolchainGuard" TaskFactory="RoslynCodeTaskFactory" AssemblyFile="$(MSBuildToolsPath)/Microsoft.Build.Tasks.Core.dll">
    <Task><Code Type="Class" Language="cs" Source="{}" /></Task>
  </UsingTask>
  <Target Name="Check">
    <WispMonoToolchainGuard UpstreamTarget="{upstream}" Sdk="$(MSBuildProjectDirectory)" />
  </Target>
</Project>"#,
        source.display()
    );
    std::fs::write(directory.join("guard.proj"), project).unwrap();
    run_task(directory, "guard.proj", "-t:Check")
}

#[test]
fn mono_toolchain_checks_refuse_missing_or_changed_workload_targets() {
    for contents in [None, Some("a different workload target")] {
        let directory = tempfile::tempdir().unwrap();
        if let Some(contents) = contents {
            std::fs::write(directory.path().join("upstream.targets"), contents).unwrap();
        }
        let result = check_toolchain(directory.path(), "upstream.targets");
        assert!(
            !result.status.success(),
            "unrecognized workload targets must not enable the Mono override"
        );
        let output = String::from_utf8_lossy(&result.stdout);
        let diagnostic = if contents.is_some() {
            "Mono WasiApp.targets changed"
        } else {
            "install the pinned wasi-experimental workload"
        };
        assert!(
            output.contains(diagnostic),
            "the workload rejection must identify its actual cause: {output}"
        );
    }
}

#[test]
fn mono_toolchain_checks_refuse_missing_empty_and_wrong_native_versions() {
    let props = include_str!("../../../guest-sdks/dotnet/Wisp.Guest.props");
    let version = props
        .split_once("<WispManagedRuntimeVersion>")
        .unwrap()
        .1
        .split_once("</WispManagedRuntimeVersion>")
        .unwrap()
        .0;
    let upstream = format!("$(MSBuildToolsPath)/../../packs/Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk/{version}/Sdk/WasiApp.targets");
    for contents in [None, Some(""), Some("unsupported-native-sdk")] {
        let directory = tempfile::tempdir().unwrap();
        if let Some(contents) = contents {
            std::fs::write(directory.path().join("VERSION"), contents).unwrap();
        }
        let result = check_toolchain(directory.path(), &upstream);
        assert!(
            !result.status.success(),
            "an unrecognized native SDK must not enable Mono linking"
        );
        let output = String::from_utf8_lossy(&result.stdout);
        assert!(output.contains("Mono core profile requires wasi-sdk"), "the pinned wasi-experimental workload must be installed and native version rejection must be actionable: {output}");
    }
}
