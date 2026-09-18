//! The platform trust bundle as a node serves it to the guest's PAL.

use std::hash::{Hash, Hasher};

use isoplane_engine::{PlatformFile, PlatformFiles};

/// Where every guest finds the platform's root bundle.
pub const BUNDLE_PATH: &str = "/etc/ssl/certs/ca-certificates.crt";

/// The PEM roots concatenated, with a timestamp derived from the content so
/// any change, a rollback included, is a change the guest's store provider
/// sees.
pub fn bundle(roots: &[&str]) -> PlatformFiles {
    let pem: String = roots.concat();
    let mut hasher = std::hash::DefaultHasher::new();
    pem.hash(&mut hasher);
    PlatformFiles::new(vec![PlatformFile {
        path: BUNDLE_PATH.into(),
        bytes: pem.into_bytes().into(),
        modified_ns: hasher.finish() % 1_000_000_000_000,
    }])
    .expect("the bundle's path is one absolute file")
}
