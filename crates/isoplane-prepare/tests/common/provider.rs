use isoplane_prepare::LocalPreparation;

/// A local provider and the directory holding its compilation cache.
pub struct LocalProvider {
    preparation: LocalPreparation,
    _cache: tempfile::TempDir,
}

impl std::ops::Deref for LocalProvider {
    type Target = LocalPreparation;

    fn deref(&self) -> &LocalPreparation {
        &self.preparation
    }
}

pub fn local_preparation() -> LocalProvider {
    let directory = tempfile::tempdir().expect("owned compiler cache");
    // SAFETY: this unique empty directory is written only by the cache's atomic compiler path.
    let cache = unsafe {
        isoplane_engine::CompilationCache::from_trusted_directory(directory.path().join("cache"))
    }
    .expect("owned cache");
    LocalProvider {
        preparation: LocalPreparation::with_cache(std::sync::Arc::new(cache))
            .expect("local provider"),
        _cache: directory,
    }
}
