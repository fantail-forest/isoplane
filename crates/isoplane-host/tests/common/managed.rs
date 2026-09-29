pub fn files(name: &str) -> isoplane_engine::PlatformFiles {
    isoplane_engine::PlatformFiles::new(
        isoplane_fixtures::managed_probe(name)
            .into_iter()
            .map(|file| isoplane_engine::PlatformFile {
                path: file.path,
                bytes: file.bytes.into(),
                modified_ns: 0,
            })
            .collect(),
    )
    .expect("valid managed fixture files")
}
