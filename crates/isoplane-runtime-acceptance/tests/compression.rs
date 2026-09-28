//! Compression through the pack's native zlib: the stream codecs round-trip
//! and reject corrupt input, zip and tar archives build and read back in
//! memory, a codec's native state survives a Store restoration between its
//! writes, and the helpers that need a directory fail on the empty disk.
//! Brotli ships without a native backend on wasi and says so.

#[macro_use]
mod common;

use common::{guest, GuestRuntime};
use isoplane_engine::Completion;
use isoplane_harness::EmittedGuestLogs as _;

async fn answer(runtime: GuestRuntime, path: &str) -> String {
    let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
    reactor.context.begin_slice(1, 1);
    let mut guest = guest(&reactor, runtime);
    guest.answer(&reactor, path, 1).await
}

in_mono_runtime! {
    #[tokio::test]
    async fn gzip_deflate_and_zlib_streams_round_trip_a_compressible_buffer(runtime: GuestRuntime) {
        for (codec, header) in [("gzip", "1f8b"), ("zlib", "78"), ("deflate", "")].into_iter() {
            let answer = answer(runtime, &format!("/compression/round-trip/{codec}")).await;
            assert!(
                answer.starts_with(header) && answer.ends_with(" smaller=True restored=True"),
                "{codec} must write its framing, shrink the pattern and restore it exactly: {answer}"
            );
        }
    }
}

in_mono_runtime! {
    /// `System.IO.Compression.Brotli` has no wasi native backend in the pack,
    /// so the codec declares itself unsupported rather than mis-encoding.
    #[tokio::test]
    async fn a_brotli_stream_round_trips_a_compressible_buffer(runtime: GuestRuntime) {
        let text = answer(runtime, "/compression/round-trip/brotli").await;
        assert!(
            text.ends_with(" smaller=True restored=True"),
            "Brotli encodes smaller and decodes back through the pack's brotli: {text}"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn bytes_that_are_not_a_gzip_member_are_invalid_data(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, "/compression/corrupt").await,
            "InvalidDataException",
            "a decoder fed plain text must reject it as invalid data, never decode garbage"
        );
    }
}

in_mono_runtime! {
    /// Upstream `DeflateStream` ends a truncated member as a short, clean
    /// stream (the long-standing .NET behaviour on every platform), so an
    /// application that needs integrity checks the length or the trailer
    /// itself. Recorded so a change in the runtime is noticed.
    #[tokio::test]
    async fn a_gzip_member_cut_before_its_trailer_decodes_short_without_an_error(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, "/compression/truncated").await,
            "decoded less",
            "a truncated member decodes to fewer bytes with no exception, as upstream .NET does"
        );
    }
}

in_mono_runtime! {
    #[tokio::test]
    async fn a_zip_archive_builds_in_memory_and_lists_its_entries_back(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, "/compression/zip").await,
            "docs/text.txt:4000:deflated:same raw.bin:1000:stored:same",
            "both entries must reopen with their names, lengths, storage method and bytes"
        );
    }
}

in_mono_runtime! {
    /// `System.Formats.Tar` ships as a throwing stub for wasi in the
    /// framework: constructing a writer is `PlatformNotSupportedException`.
    #[tokio::test]
    async fn a_pax_tar_archive_round_trips_in_memory(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, "/compression/tar").await,
            "dir/:Directory:Pax:0: dir/hello.txt:RegularFile:Pax:14:hello from tar",
            "a PAX archive written to memory reads back entry by entry with its data"
        );
    }

    #[tokio::test]
    async fn extracting_a_tar_archive_to_the_disk_is_refused_by_the_empty_filesystem(runtime: GuestRuntime) {
        let text = answer(runtime, "/compression/tar-disk").await;
        assert!(
            matches!(text.as_str(), "IOException" | "DirectoryNotFoundException" | "UnauthorizedAccessException"),
            "extraction needs the disk and meets the read-only filesystem's refusal, never a trap or a file: {text}"
        );
    }
}

in_mono_runtime! {
    /// `ZipFile` needs a file on disk and meets the empty read-only
    /// filesystem: excluded, and the failure is the disk's, not a trap.
    #[tokio::test]
    async fn opening_a_zip_file_on_the_disk_is_refused_by_the_empty_filesystem(runtime: GuestRuntime) {
        assert_eq!(
            answer(runtime, "/compression/disk").await,
            "IOException",
            "ZipFile.Open must fail with the disk's refusal, never create a file"
        );
    }
}

in_mono_runtime! {
    /// An encoder is fed four chunks with a timer park between each, the
    /// guest parks once more, then decodes in four exact reads with a park
    /// between each: nine parks, each crossing a Store restoration with live
    /// zlib state in the guest's memory.
    #[tokio::test(start_paused = true)]
    async fn a_gzip_stream_mid_write_survives_a_store_restoration_delta_at_every_park(runtime: GuestRuntime) {
        let reactor = isoplane_harness::Reactor::new(isoplane_host::InstanceId::alloc());
        reactor.context.begin_slice(1, 1);
        let mut guest = guest(&reactor, runtime);
        let mut outcome = guest.start("/compression/gzip-across-parks");
        let mut wakes = 0;
        while outcome.completion == Completion::Parked {
            wakes += 1;
            assert!(wakes <= 9, "nine timer parks need exactly nine wakes; logs: {:?}", reactor.context.emitted_logs());
            tokio::task::yield_now().await;
            tokio::time::advance(std::time::Duration::from_millis(20)).await;
            reactor.context.begin_slice(1 + 20 * wakes, 1);
            outcome = guest.wake(&reactor).await;
        }
        assert_eq!(
            outcome.completion,
            Completion::Done,
            "the dispatch must complete after its parks: {outcome:?}; logs: {:?}",
            reactor.context.emitted_logs()
        );
        assert_eq!(wakes, 9, "every one of the nine parks must be a real park on a host timer");
        assert_eq!(
            reactor.response_text(1),
            "compressed=True restored=True",
            "the bytes encoded across the parks must decode back across the parks"
        );
    }
}
