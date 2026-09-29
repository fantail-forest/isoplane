use isoplane_engine::{PlatformFile, PlatformFiles};

pub fn probe_files(name: &str) -> PlatformFiles {
    let files = isoplane_fixtures::managed_probe(name)
        .into_iter()
        .map(|file| PlatformFile {
            path: file.path,
            bytes: file.bytes.into(),
            modified_ns: 0,
        })
        .collect();
    PlatformFiles::new(files).expect("valid managed probe files")
}
