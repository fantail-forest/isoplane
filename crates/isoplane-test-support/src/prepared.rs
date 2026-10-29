//! Where tests find the repository and what Task prepared for them.

use std::path::{Path, PathBuf};

/// The repository root above a crate's `CARGO_MANIFEST_DIR`: every crate
/// lives in `crates/<name>`. Pass `env!("CARGO_MANIFEST_DIR")`, which
/// expands in the calling crate. The path is not canonicalised, so that a
/// tool given it never sees Windows' extended-length syntax.
pub fn repository_root(manifest_dir: &str) -> PathBuf {
    Path::new(manifest_dir)
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("{manifest_dir} is not a crate under crates/"))
        .to_path_buf()
}

/// The path Task exports as `variable`, failing the test with `task` to run
/// when the tests run without it.
pub fn prepared_path(variable: &str, task: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!("{variable} is unset; run {task}, and run the tests through Task")
        })
}

#[cfg(test)]
mod tests {
    use super::{prepared_path, repository_root};
    use crate::EnvironmentVariable;

    #[test]
    fn the_repository_root_holds_the_workspace_manifest() {
        let root = repository_root(env!("CARGO_MANIFEST_DIR"));
        assert!(
            root.join("Cargo.toml").is_file() && root.join("crates").is_dir(),
            "{} is not the repository",
            root.display()
        );
    }

    #[test]
    fn a_prepared_path_is_read_from_its_variable_and_names_the_task_when_unset() {
        let name = format!("ISOPLANE_TEST_SUPPORT_PREPARED_{}", std::process::id());
        let _set = EnvironmentVariable::set(&name, "/prepared/by/task");
        assert_eq!(
            prepared_path(&name, "task fixtures:example"),
            std::path::Path::new("/prepared/by/task")
        );
        let missing = format!("{name}_MISSING");
        let failed = std::panic::catch_unwind(|| prepared_path(&missing, "task fixtures:example"));
        let message = failed
            .expect_err("an unset variable must fail the test")
            .downcast::<String>()
            .expect("the failure is a formatted message");
        assert!(
            message.contains(&missing) && message.contains("task fixtures:example"),
            "the failure names the variable and the task: {message}"
        );
    }
}
