use std::sync::Arc;

use isoplane_engine::IsoplaneEngine;
use isoplane_prepare::{BuildRequest, LocalPreparation, Preparation, PrepareError};

use super::{load, Prepared};

/// A local preparation provider and the directory holding its compilation cache.
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

/// A local preparation provider with an empty compilation cache of its own.
pub fn local_preparation() -> LocalProvider {
    let directory = tempfile::tempdir().expect("a compilation cache directory");
    // SAFETY: only the cache's compiler writes into this new, private directory.
    let cache = unsafe {
        isoplane_engine::CompilationCache::from_trusted_directory(directory.path().join("cache"))
    }
    .expect("the compilation cache must open");
    LocalProvider {
        preparation: LocalPreparation::with_cache(Arc::new(cache))
            .expect("the local provider must start"),
        _cache: directory,
    }
}

/// Prepares the module `request` carries, one that carries its whole
/// application as a guest built with the Rust SDK does, the way a node
/// prepares one: its whole initialization as one layer.
pub async fn prepare_whole_module(request: &BuildRequest) -> Result<Prepared, PrepareError> {
    let product = local_preparation()
        .prepare(request, &tokio_util::sync::CancellationToken::new())
        .await?;
    let engine = Arc::new(
        IsoplaneEngine::new(request.engine.clone()).expect("the request's engine must start"),
    );
    let template = load(
        &engine,
        &product,
        request.identity().expect("the module's identity"),
        None,
    );
    Ok(Prepared {
        template,
        initialized: product.initialized,
        engine,
    })
}
