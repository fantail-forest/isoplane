//! One content-addressed pre-initialized WASM image per local application.

use std::fs::File;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;

use super::Project;

/// Reuses a verified image for the current preparation inputs, or publishes a new one.
/// Only sandbox bytecode is cached; native compilation and admission still run normally.
pub(super) fn template(
    project: &Project,
    wasm: &[u8],
    prepare: impl FnOnce() -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let executable = std::env::current_exe().context("locating the local runtime executable")?;
    Cache::new(project, wasm, &executable)?.get_or_prepare(prepare)
}

struct Cache {
    path: PathBuf,
    key: blake3::Hash,
    budgets: wisp_abi::config::Limits,
    guest_memory_bytes: Option<wisp_abi::config::GuestMemoryBytes>,
}

impl Cache {
    fn new(project: &Project, wasm: &[u8], executable: &Path) -> anyhow::Result<Self> {
        let mut runtime = blake3::Hasher::new();
        runtime
            .update_reader(File::open(executable)?)
            .with_context(|| format!("identifying local runtime {}", executable.display()))?;
        let mut key = blake3::Hasher::new_derive_key("wisp local preinitialization cache v1");
        for bytes in [
            wasm,
            runtime.finalize().as_bytes(),
            &serde_json::to_vec(&project.manifest)?,
            project.init_sandbox.as_str().as_bytes(),
        ] {
            hash_part(&mut key, bytes);
        }
        for file in project.platform_files.files() {
            hash_part(&mut key, file.path.as_bytes());
            hash_part(&mut key, &file.bytes);
            hash_part(&mut key, &file.modified_ns.to_le_bytes());
        }
        Ok(Self {
            path: project.state_dir().join("preinitialized.wasm-cache"),
            key: key.finalize(),
            budgets: project.manifest.limits,
            guest_memory_bytes: project.manifest.guest_memory_bytes,
        })
    }

    fn get_or_prepare(
        &self,
        prepare: impl FnOnce() -> anyhow::Result<Vec<u8>>,
    ) -> anyhow::Result<Vec<u8>> {
        if let Some(wasm) = self.read()? {
            match wisp_runtime::validate_with_memory_limit(
                &wasm,
                self.budgets,
                self.guest_memory_bytes,
            ) {
                Ok(report) if !report.has_initialize() => {
                    tracing::info!(path = %self.path.display(), "reusing local pre-initialized WASM");
                    return Ok(wasm);
                }
                Ok(_) => {
                    tracing::warn!(path = %self.path.display(), "rebuilding local preparation that still exports initialization")
                }
                Err(violations) => {
                    tracing::warn!(path = %self.path.display(), ?violations, "rebuilding invalid local preparation")
                }
            }
        }
        let wasm = prepare()?;
        self.publish(&wasm)?;
        Ok(wasm)
    }

    fn read(&self) -> anyhow::Result<Option<Vec<u8>>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading local preparation {}", self.path.display()))
            }
        };
        if bytes.len() < 64 {
            tracing::warn!(path = %self.path.display(), "rebuilding truncated local preparation");
            return Ok(None);
        }
        if &bytes[..32] != self.key.as_bytes() {
            return Ok(None);
        }
        let wasm = &bytes[64..];
        if &bytes[32..64] != blake3::hash(wasm).as_bytes() {
            tracing::warn!(path = %self.path.display(), "rebuilding corrupt local preparation");
            return Ok(None);
        }
        Ok(Some(wasm.to_vec()))
    }

    fn publish(&self, wasm: &[u8]) -> anyhow::Result<()> {
        let directory = self
            .path
            .parent()
            .context("locating local preparation directory")?;
        std::fs::create_dir_all(directory)?;
        let mut pending = tempfile::NamedTempFile::new_in(directory)?;
        pending.write_all(self.key.as_bytes())?;
        pending.write_all(blake3::hash(wasm).as_bytes())?;
        pending.write_all(wasm)?;
        pending.flush()?;
        pending
            .persist(&self.path)
            .with_context(|| format!("publishing local preparation {}", self.path.display()))?;
        Ok(())
    }
}

fn hash_part(hash: &mut blake3::Hasher, bytes: &[u8]) {
    hash.update(&(bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use wisp_abi::config::{InitSandbox, Limits};
    use wisp_engine::{PlatformFile, PlatformFiles};

    struct Fixture {
        _directory: tempfile::TempDir,
        project: Project,
        executable: PathBuf,
        wasm: Vec<u8>,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            std::fs::write(
                directory.path().join("wisp.toml"),
                "name='app'\ntenant='owner'\nmodule='app.wasm'",
            )
            .unwrap();
            let project = Project::load(directory.path(), None, None, true).unwrap();
            let executable = directory.path().join("runtime");
            std::fs::write(&executable, b"runtime version one").unwrap();
            let wasm = wat::parse_str(
                r#"(module
                (memory (export "memory") 1 1)
                (func (export "wisp_initialize")
                    (i32.store (i32.const 0) (i32.const 42)))
                (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
                (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
                (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1)))"#,
            )
            .unwrap();
            Self {
                _directory: directory,
                project,
                executable,
                wasm,
            }
        }

        fn cache(&self) -> Cache {
            Cache::new(&self.project, &self.wasm, &self.executable).unwrap()
        }
    }

    #[test]
    fn unchanged_inputs_reuse_a_real_preinitialized_template() {
        let fixture = Fixture::new();
        super::super::validate(&fixture.project, &fixture.wasm, false).unwrap();
        let first = fixture
            .cache()
            .get_or_prepare(|| super::super::preinitialize(&fixture.project, &fixture.wasm))
            .unwrap();
        let cached = fixture
            .cache()
            .get_or_prepare(|| {
                panic!("an unchanged local restart must not run initialization again")
            })
            .unwrap();
        assert_eq!(
            cached, first,
            "a cache hit must return the initialized image unchanged"
        );
        let engine = wisp_engine::WispEngine::new(wisp_engine::EngineConfig {
            pooling: false,
            ..Default::default()
        })
        .unwrap();
        let template = engine.load_template(&cached).unwrap();
        assert_ne!(
            cached, fixture.wasm,
            "preparation must retain the initialized image, not the original module"
        );
        assert_eq!(
            template.hash,
            wisp_engine::ModuleHash::of(&first),
            "the ordinary engine must load the cached sandbox bytecode"
        );
    }

    #[test]
    fn an_independent_memory_override_is_retained_when_reusing_local_preparation() {
        let mut fixture = Fixture::new();
        fixture.project.manifest.guest_memory_bytes =
            Some(wisp_abi::config::GuestMemoryBytes::new(512 << 20).unwrap());
        fixture.wasm = wat::parse_str(
            r#"(module
                (memory (export "memory") 1 8192)
                (func (export "wisp_initialize"))
                (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
                (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
                (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1)))"#,
        )
        .unwrap();
        super::super::validate(&fixture.project, &fixture.wasm, false).unwrap();
        let first = fixture
            .cache()
            .get_or_prepare(|| super::super::preinitialize(&fixture.project, &fixture.wasm))
            .unwrap();
        let cached = fixture
            .cache()
            .get_or_prepare(|| panic!("the same memory authorization must reuse initialization"))
            .unwrap();
        assert_eq!(
            cached, first,
            "cached preparation must preserve the authorized memory ceiling"
        );
        assert_eq!(
            fixture.project.manifest.limits,
            wisp_abi::config::Limits {
                module_bytes_compressed: 20971520,
                instance_memory_max: 134217728,
                cpu_ms_per_http_dispatch: 30000,
                subrequests_per_dispatch: 200,
                delta_storage_bytes: 10737418240,
                pending_futures_per_dispatch: 64
            },
            "reusing a large memory template must not require an larger limits selection"
        );
    }

    #[test]
    fn preparation_inputs_invalidate_the_existing_image() {
        let fixture = Fixture::new();
        let original = fixture.cache();
        original.publish(b"cached template").unwrap();
        let assert_miss = |project: &Project, wasm: &[u8], executable: &Path, reason: &str| {
            let cache = Cache::new(project, wasm, executable).unwrap();
            assert!(
                cache.read().unwrap().is_none(),
                "{reason} must invalidate local preparation"
            );
        };
        let mut changed = fixture.wasm.clone();
        changed.push(0);
        assert_miss(
            &fixture.project,
            &changed,
            &fixture.executable,
            "changed module bytes",
        );
        let mut project = fixture.project.clone();
        project
            .manifest
            .init_visible_vars
            .push(("name".into(), "value".into()));
        assert_miss(
            &project,
            &fixture.wasm,
            &fixture.executable,
            "changed initialization variables",
        );
        project = fixture.project.clone();
        project.init_sandbox = InitSandbox::RuntimeBringUp;
        assert_miss(
            &project,
            &fixture.wasm,
            &fixture.executable,
            "changed initialization sandbox",
        );
        project = fixture.project.clone();
        project.manifest.limits = wisp_abi::config::Limits {
            module_bytes_compressed: 5242880,
            instance_memory_max: 67108864,
            cpu_ms_per_http_dispatch: 10,
            subrequests_per_dispatch: 20,
            pending_futures_per_dispatch: 6,
        };
        assert_miss(
            &project,
            &fixture.wasm,
            &fixture.executable,
            "changed admission limits",
        );
        for change in ["path", "bytes", "timestamp", "removed"] {
            project = fixture.project.clone();
            let mut files = project.platform_files.files().to_vec();
            match change {
                "path" => files[0].path = "/other-ca.crt".into(),
                "bytes" => files[0].bytes = (&b"rotated trust roots"[..]).into(),
                "timestamp" => files[0].modified_ns += 1,
                "removed" => files.clear(),
                _ => panic!("every fixture change has an explicit mutation"),
            }
            project.platform_files = PlatformFiles::new(files).unwrap();
            assert_miss(&project, &fixture.wasm, &fixture.executable, change);
        }
        project = fixture.project.clone();
        let mut files = project.platform_files.files().to_vec();
        files.push(PlatformFile {
            path: "/new-file".into(),
            bytes: (&b"new"[..]).into(),
            modified_ns: 0,
        });
        project.platform_files = PlatformFiles::new(files).unwrap();
        assert_miss(
            &project,
            &fixture.wasm,
            &fixture.executable,
            "an additional platform file",
        );
        std::fs::write(&fixture.executable, b"runtime version two").unwrap();
        assert_miss(
            &fixture.project,
            &fixture.wasm,
            &fixture.executable,
            "changed executable bytes at the same path",
        );
    }

    #[test]
    fn corrupt_and_truncated_images_are_replaced_by_fresh_preparation() {
        let fixture = Fixture::new();
        let cache = fixture.cache();
        for truncated in [false, true] {
            cache.publish(b"original image").unwrap();
            let mut bytes = std::fs::read(&cache.path).unwrap();
            if truncated {
                bytes.truncate(8);
            } else {
                bytes[64] ^= 1;
            }
            std::fs::write(&cache.path, bytes).unwrap();
            let rebuilt = cache
                .get_or_prepare(|| Ok(b"rebuilt image".to_vec()))
                .unwrap();
            assert_eq!(
                rebuilt, b"rebuilt image",
                "damaged cached bytecode must trigger fresh preparation"
            );
            assert_eq!(
                cache.read().unwrap().unwrap(),
                rebuilt,
                "recovery must atomically replace the damaged image"
            );
        }
    }

    #[test]
    fn failed_preparation_preserves_the_last_complete_entry() {
        let mut fixture = Fixture::new();
        let previous = fixture.cache();
        previous.publish(b"previous image").unwrap();
        fixture.wasm.push(0);
        let result = fixture
            .cache()
            .get_or_prepare(|| anyhow::bail!("initialization rejected"));
        assert!(
            result.is_err(),
            "a preparation failure must propagate to the caller"
        );
        assert_eq!(
            previous.read().unwrap().unwrap(),
            b"previous image",
            "a failed build must not overwrite the last complete entry"
        );
    }

    #[test]
    fn concurrent_publications_leave_one_complete_entry() {
        let fixture = Fixture::new();
        let first = fixture.cache();
        let second = fixture.cache();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                first.publish(b"first image").unwrap();
            });
            scope.spawn(|| {
                barrier.wait();
                second.publish(b"second image").unwrap();
            });
        });
        let image = first.read().unwrap().unwrap();
        assert!(
            image == b"first image" || image == b"second image",
            "concurrent publication must leave a checksum-valid, complete image"
        );
        assert_eq!(
            std::fs::read_dir(first.path.parent().unwrap())
                .unwrap()
                .count(),
            1,
            "publication must retain one cache entry and clean temporary files"
        );
    }

    #[test]
    fn a_matching_checksum_cannot_authorize_forbidden_cached_capabilities() {
        let fixture = Fixture::new();
        let cache = fixture.cache();
        let hostile = wat::parse_str(
            r#"(module
            (import "outside" "io" (func))
            (memory (export "memory") 1 1)
            (func (export "wisp_alloc") (param i32 i32) (result i32) (i32.const 0))
            (func (export "wisp_handle") (param i32 i32) (result i64) (i64.const 1))
            (func (export "wisp_wake") (param i64 i32 i32) (result i64) (i64.const 1)))"#,
        )
        .unwrap();
        let violations =
            wisp_runtime::validate(&hostile, fixture.project.manifest.limits).unwrap_err();
        assert_eq!(
            violations.len(),
            1,
            "the cached fixture must violate only the import policy"
        );
        assert_eq!(
            violations[0].code,
            wisp_runtime::ViolationCode::V1UndeclaredImport,
            "cache recovery must exercise the forbidden capability guard"
        );
        cache.publish(&hostile).unwrap();
        let expected = super::super::preinitialize(&fixture.project, &fixture.wasm).unwrap();
        let rebuilt = cache.get_or_prepare(|| Ok(expected.clone())).unwrap();
        assert_eq!(
            rebuilt, expected,
            "cached imports must pass admission even when their checksum matches"
        );
        assert_eq!(
            cache.read().unwrap().unwrap(),
            expected,
            "recovery must replace forbidden cached code with a freshly initialized image"
        );
    }

    #[test]
    fn an_uninitialized_cached_module_is_rebuilt() {
        let fixture = Fixture::new();
        let cache = fixture.cache();
        cache.publish(&fixture.wasm).unwrap();
        let expected = super::super::preinitialize(&fixture.project, &fixture.wasm).unwrap();
        let rebuilt = cache.get_or_prepare(|| Ok(expected.clone())).unwrap();
        assert_eq!(
            rebuilt, expected,
            "cached bytecode must have completed initialization"
        );
    }
}
