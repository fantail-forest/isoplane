use super::Recorder;
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

impl Recorder {
    pub fn take_ready(&self, dispatch: u64) -> Option<(u64, isoplane_abi::result::ResultRecord)> {
        let mut slices = self.ready_slices.lock();
        if let Some(index) = slices.iter().position(|(owner, _)| *owner == dispatch) {
            let (_, future) = slices
                .remove(index)
                .expect("the matching slice is in the queue");
            return Some((future, isoplane_abi::result::ResultRecord::Ok(Vec::new())));
        }
        self.response_writes
            .lock()
            .pop_front()
            .map(|(future, bytes)| {
                (
                    future,
                    isoplane_abi::result::ResultRecord::Ok(
                        postcard::to_allocvec(&bytes).expect("write byte count"),
                    ),
                )
            })
    }
}
