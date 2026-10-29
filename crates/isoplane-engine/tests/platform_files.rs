//! Host-declared platform files through the engine's real validator and
//! linker: every allowed operation on a declared file and its implied
//! directories, and every rejection boundary around them.
mod common;
#[path = "common/filesystem.rs"]
mod filesystem;
use isoplane_test_support::published_files;
use std::sync::Arc;

use common::{activate, small_engine_config, test_engine, DONE};
use filesystem::{call, dispatch, RET, ROOT};
use isoplane_abi::filesystem::Operation;
use isoplane_abi::imports::MAX_OUT_BUFFER_BYTES;
use isoplane_engine::{
    Completion, EngineConfig, InstanceOps, IsoplaneEngine, OccupancyBudget, PlatformFile,
    PlatformFiles,
};

const BUNDLE: &[u8] = b"-----BEGIN CERTIFICATE-----\nMIIBisoplane\n-----END CERTIFICATE-----\n";
const BUNDLE_PATH: &str = "etc/ssl/certs/ca-certificates.crt";
const BUNDLE_MODIFIED_NS: u64 = 1_757_548_800_000_000_042;
const HOSTNAME: &[u8] = b"isoplane\n";
const BIG_TAIL: usize = 17;

const REGULAR_FILE: u8 = 6;
const DIRECTORY: u8 = 3;
const BAD_DESCRIPTOR: u8 = 3;
const INVALID: u8 = 12;
const IS_DIRECTORY: u8 = 14;
const NO_ENTRY: u8 = 20;
const NOT_DIRECTORY: u8 = 24;
const NOT_PERMITTED: u8 = 31;
const READ_ONLY: u8 = 33;

/// Every path the tests name, laid out 64 bytes apart from 8000, above
/// every result area.
const STRINGS: &[&str] = &[
    ".",
    "..",
    "etc",
    "ssl",
    "certs",
    "ca-certificates.crt",
    BUNDLE_PATH,
    "etc/ssl/certs",
    "etc/ssl",
    "etc/hostname",
    "usr/share/big.bin",
    "usr",
    "etc/passwd",
    "var",
    "etc/ssl/private",
    "usr/share/zoneinfo",
    "../etc/ssl",
    "../../../../etc/hostname",
    "etc/ssl/certs/ca-certificates.crt/x",
];

fn strings() -> Vec<(u32, &'static str)> {
    STRINGS
        .iter()
        .enumerate()
        .map(|(index, text)| (8000 + index as u32 * 64, *text))
        .collect()
}

/// `(pointer, length)` argument text for a named path.
fn path(text: &str) -> String {
    let (offset, _) = strings()
        .into_iter()
        .find(|(_, candidate)| *candidate == text)
        .unwrap_or_else(|| panic!("`{text}` is not in the fixture's string table"));
    format!("(i32.const {offset}) (i32.const {})", text.len())
}

fn declared() -> PlatformFiles {
    let mut big = vec![0xAB; MAX_OUT_BUFFER_BYTES + BIG_TAIL];
    big[MAX_OUT_BUFFER_BYTES..].fill(0xCD);
    PlatformFiles::new(vec![
        PlatformFile {
            path: format!("/{BUNDLE_PATH}"),
            bytes: Arc::from(BUNDLE),
            modified_ns: BUNDLE_MODIFIED_NS,
        },
        PlatformFile {
            path: "/etc/hostname".into(),
            bytes: Arc::from(HOSTNAME),
            modified_ns: 0,
        },
        PlatformFile {
            path: "/usr/share/big.bin".into(),
            bytes: Arc::from(big),
            modified_ns: 1,
        },
    ])
    .expect("the fixture set is well formed")
}

fn engine_with(files: PlatformFiles) -> IsoplaneEngine {
    IsoplaneEngine::new(EngineConfig {
        platform_files: files,
        ..small_engine_config()
    })
    .expect("engine")
}

fn run(body: &str) -> Vec<u8> {
    filesystem::run(
        &engine_with(declared()),
        &filesystem::module(body, &strings()),
    )
}

fn word(memory: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(memory[at..at + 4].try_into().expect("four bytes"))
}

fn quad(memory: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(memory[at..at + 8].try_into().expect("eight bytes"))
}

/// The bytes of a canonical `list<u8>` whose `(pointer, length)` sits at `at`.
fn list(memory: &[u8], at: usize) -> &[u8] {
    let (pointer, length) = (word(memory, at) as usize, word(memory, at + 4) as usize);
    &memory[pointer..pointer + length]
}

fn ret(at: u32) -> String {
    format!("(i32.const {at})")
}

/// The descriptor an `open-at` wrote into the result area at `at`.
fn opened(at: u32) -> String {
    format!("(i32.load (i32.const {}))", at + 4)
}

fn open_at(from: &str, text: &str, open_flags: u32, descriptor_flags: u32, at: u32) -> String {
    call(
        Operation::OpenAt,
        &[
            from,
            "(i32.const 0)",
            &path(text),
            &format!("(i32.const {open_flags})"),
            &format!("(i32.const {descriptor_flags})"),
            &ret(at),
        ],
    )
}

fn open(from: &str, text: &str, at: u32) -> String {
    open_at(from, text, 0, 0, at)
}

fn stat_at(from: &str, text: &str, at: u32) -> String {
    call(
        Operation::StatAt,
        &[from, "(i32.const 0)", &path(text), &ret(at)],
    )
}

fn read(descriptor: &str, length: i64, offset: i64, at: u32) -> String {
    call(
        Operation::Read,
        &[
            descriptor,
            &format!("(i64.const {length})"),
            &format!("(i64.const {offset})"),
            &ret(at),
        ],
    )
}

fn assert_ok(memory: &[u8], at: usize, what: &str) {
    assert_eq!(
        memory[at],
        0,
        "{what} must succeed (error code {})",
        memory[at + 8]
    );
}

fn assert_error(memory: &[u8], at: usize, offset: usize, code: u8, what: &str) {
    assert_eq!(
        (memory[at], memory[at + offset]),
        (1, code),
        "{what} must fail with error code {code}"
    );
}

#[test]
fn the_declared_file_opens_stepwise_and_in_one_path_to_the_same_node() {
    let body = format!(
        "{} {} {} {} {} {} {}",
        open(ROOT, "etc", 1200),
        open(&opened(1200), "ssl", 1300),
        open(&opened(1300), "certs", 1400),
        open(&opened(1400), "ca-certificates.crt", 1500),
        open(ROOT, BUNDLE_PATH, 1600),
        call(Operation::Hash, &[&opened(1500), &ret(1700)]),
        call(Operation::Hash, &[&opened(1600), &ret(1800)]),
    );
    let memory = run(&body);
    for at in [1200, 1300, 1400, 1500, 1600] {
        assert_ok(&memory, at, "each open along the declared path");
    }
    assert_ok(&memory, 1700, "the stepwise descriptor's hash");
    assert_eq!(
        &memory[1708..1724],
        &memory[1808..1824],
        "the stepwise and the one-path open must name the same file"
    );
}

#[test]
fn a_file_stat_reports_a_regular_file_with_its_size_and_modified_time() {
    let body = format!(
        "{} {} {}",
        open(ROOT, BUNDLE_PATH, 1200),
        call(Operation::Stat, &[&opened(1200), &ret(1300)]),
        stat_at(ROOT, BUNDLE_PATH, 1500),
    );
    let memory = run(&body);
    assert_ok(&memory, 1300, "stat of the declared file");
    assert_eq!(
        memory[1308], REGULAR_FILE,
        "the declared file is a regular file"
    );
    assert_eq!(quad(&memory, 1316), 1, "the declared file has one link");
    assert_eq!(
        quad(&memory, 1324),
        BUNDLE.len() as u64,
        "the size is the declared byte count"
    );
    for (name, base) in [
        ("access", 1332),
        ("modification", 1356),
        ("status-change", 1380),
    ] {
        assert_eq!(memory[base], 1, "the {name} timestamp is present");
        assert_eq!(
            (quad(&memory, base + 8), word(&memory, base + 16)),
            (
                BUNDLE_MODIFIED_NS / 1_000_000_000,
                (BUNDLE_MODIFIED_NS % 1_000_000_000) as u32
            ),
            "the {name} timestamp is the declared modification time"
        );
    }
    assert_eq!(
        &memory[1300..1404],
        &memory[1500..1604],
        "stat-at from the root must report exactly what stat on the open file reports"
    );
}

#[test]
fn implied_directories_stat_as_directories() {
    let body = format!(
        "{} {} {} {}",
        stat_at(ROOT, "etc", 1200),
        stat_at(ROOT, "etc/ssl", 1400),
        open(ROOT, "etc/ssl/certs", 1600),
        call(Operation::Stat, &[&opened(1600), &ret(1700)]),
    );
    let memory = run(&body);
    for at in [1200, 1400, 1700] {
        assert_ok(&memory, at, "stat of an implied directory");
        assert_eq!(
            memory[at + 8],
            DIRECTORY,
            "an implied directory is a directory"
        );
        assert_eq!(
            quad(&memory, at + 16),
            1,
            "an implied directory has one link"
        );
        assert_eq!(
            quad(&memory, at + 24),
            0,
            "an implied directory has no size"
        );
    }
}

#[test]
fn the_file_reads_whole_as_a_middle_slice_and_past_its_end() {
    let length = BUNDLE.len() as i64;
    let body = format!(
        "{} {} {} {} {}",
        open(ROOT, BUNDLE_PATH, 1200),
        read(&opened(1200), 4096, 0, 1300),
        read(&opened(1200), 5, 11, 1400),
        read(&opened(1200), 10, length, 1500),
        read(&opened(1200), 10, length - 3, 1600),
    );
    let memory = run(&body);
    assert_ok(&memory, 1300, "a whole-file read");
    assert_eq!(
        list(&memory, 1304),
        BUNDLE,
        "a read from 0 returns the declared bytes"
    );
    assert_eq!(memory[1312], 1, "a read reaching the end reports eof");
    assert_ok(&memory, 1400, "a slice read");
    assert_eq!(
        list(&memory, 1404),
        b"CERTI",
        "a read from an offset returns that slice"
    );
    assert_eq!(
        memory[1412], 0,
        "a slice short of the end does not report eof"
    );
    assert_ok(&memory, 1500, "a read at the end");
    assert_eq!(
        (word(&memory, 1504), word(&memory, 1508), memory[1512]),
        (0, 0, 1),
        "a read at the end is the empty list with eof"
    );
    assert_ok(&memory, 1600, "a read over the end");
    assert_eq!(
        (list(&memory, 1604), memory[1612]),
        (&BUNDLE[BUNDLE.len() - 3..], 1),
        "a read over the end returns what remains with eof"
    );
}

#[test]
fn a_read_longer_than_the_out_buffer_cap_is_capped_without_losing_the_tail() {
    let body = format!(
        "{} {} {}",
        open(ROOT, "usr/share/big.bin", 1200),
        read(&opened(1200), -1, 0, 1300),
        read(&opened(1200), 4096, MAX_OUT_BUFFER_BYTES as i64, 1400),
    );
    let memory = run(&body);
    assert_ok(&memory, 1300, "a read asking for everything");
    let first = list(&memory, 1304);
    assert_eq!(
        first.len(),
        MAX_OUT_BUFFER_BYTES,
        "one read returns at most the ABI's out-buffer cap"
    );
    assert!(
        first.iter().all(|byte| *byte == 0xAB),
        "the capped read is the file's head"
    );
    assert_eq!(memory[1312], 0, "a capped read does not report eof");
    assert_ok(&memory, 1400, "the read after the cap");
    assert_eq!(
        list(&memory, 1404),
        &[0xCD; BIG_TAIL],
        "the next read continues from the cap to the end"
    );
    assert_eq!(memory[1412], 1, "the tail read reports eof");
}

/// Enumerate the directory open at `from` into entries at `at`, `at + 100`,
/// ... until EOF, and return the `(name, type)` pairs.
fn listing(from: &str, at: u32, slots: u32) -> String {
    let stream = call(Operation::ReadDirectory, &[from, &ret(at)]);
    let entries = (0..slots)
        .map(|slot| {
            call(
                Operation::DirectoryEntry,
                &[&opened(at), &ret(at + 100 + slot * 100)],
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{stream} {entries}")
}

fn entries(memory: &[u8], at: usize, slots: usize) -> Vec<(String, u8)> {
    let mut out = Vec::new();
    for slot in 0..slots {
        let entry = at + 100 + slot * 100;
        assert_eq!(memory[entry], 0, "every entry read succeeds");
        if memory[entry + 4] == 0 {
            return out;
        }
        out.push((
            String::from_utf8_lossy(list(memory, entry + 12)).into_owned(),
            memory[entry + 8],
        ));
    }
    panic!("the listing at {at} did not reach EOF within {slots} entries");
}

#[test]
fn directory_listings_enumerate_implied_children_in_name_order_then_eof() {
    let body = format!(
        "{} {} {} {} {}",
        listing(ROOT, 1200, 3),
        open(ROOT, "etc", 3000),
        listing(&opened(3000), 3100, 3),
        open(ROOT, "etc/ssl/certs", 5000),
        listing(&opened(5000), 5100, 2),
    );
    let memory = run(&body);
    assert_eq!(
        entries(&memory, 1200, 3),
        [
            ("etc".to_string(), DIRECTORY),
            ("usr".to_string(), DIRECTORY)
        ],
        "the root lists its implied directories in name order"
    );
    assert_eq!(
        entries(&memory, 3100, 3),
        [
            ("hostname".to_string(), REGULAR_FILE),
            ("ssl".to_string(), DIRECTORY)
        ],
        "a directory lists files and subdirectories in name order"
    );
    assert_eq!(
        entries(&memory, 5100, 2),
        [("ca-certificates.crt".to_string(), REGULAR_FILE)],
        "the leaf directory lists the declared file then EOF"
    );
}

#[test]
fn every_node_has_a_distinct_metadata_hash_by_descriptor_and_by_path() {
    let hash_at = |text: &str, at: u32| {
        call(
            Operation::HashAt,
            &[ROOT, "(i32.const 0)", &path(text), &ret(at)],
        )
    };
    let body = format!(
        "{} {} {} {} {} {} {} {}",
        call(Operation::Hash, &[ROOT, &ret(1200)]),
        hash_at("etc", 1300),
        hash_at("etc/ssl", 1400),
        hash_at("etc/ssl/certs", 1500),
        hash_at(BUNDLE_PATH, 1600),
        hash_at("etc/hostname", 1700),
        open(ROOT, BUNDLE_PATH, 1800),
        call(Operation::Hash, &[&opened(1800), &ret(1900)]),
    );
    let memory = run(&body);
    let mut seen = Vec::new();
    for at in [1200, 1300, 1400, 1500, 1600, 1700] {
        assert_ok(&memory, at, "a metadata hash");
        let hash = memory[at + 8..at + 24].to_vec();
        assert!(
            !seen.contains(&hash),
            "no two nodes may share a metadata hash, or a store provider would dedup them"
        );
        seen.push(hash);
    }
    assert_eq!(
        &memory[1608..1624],
        &memory[1908..1924],
        "the hash of a path is the hash of the descriptor opened on it"
    );
}

#[test]
fn type_and_flags_describe_a_read_only_regular_file() {
    let body = format!(
        "{} {} {} {} {}",
        open(ROOT, BUNDLE_PATH, 1200),
        call(Operation::Type, &[&opened(1200), &ret(1300)]),
        call(Operation::Flags, &[&opened(1200), &ret(1400)]),
        open(ROOT, "etc/ssl", 1500),
        call(Operation::Type, &[&opened(1500), &ret(1600)]),
    );
    let memory = run(&body);
    assert_eq!(
        &memory[1300..1302],
        &[0, REGULAR_FILE],
        "get-type on the declared file is regular-file"
    );
    assert_eq!(
        &memory[1400..1402],
        &[0, 1],
        "get-flags on the declared file is read"
    );
    assert_eq!(
        &memory[1600..1602],
        &[0, DIRECTORY],
        "get-type on an implied directory is directory"
    );
}

#[test]
fn sync_and_advise_on_the_file_are_no_ops() {
    let body = format!(
        "{} {} {}",
        open(ROOT, BUNDLE_PATH, 1200),
        call(Operation::Sync, &[&opened(1200), &ret(1300)]),
        call(
            Operation::Advise,
            &[
                &opened(1200),
                "(i64.const 0)",
                "(i64.const 0)",
                "(i32.const 0)",
                &ret(1400)
            ]
        ),
    );
    let memory = run(&body);
    assert_eq!(memory[1300], 0, "sync on a read-only file succeeds");
    assert_eq!(memory[1400], 0, "advise on a read-only file succeeds");
}

#[test]
fn undeclared_siblings_and_parents_are_no_entry() {
    for text in ["etc/passwd", "var", "etc/ssl/private", "usr/share/zoneinfo"] {
        let body = format!("{} {}", stat_at(ROOT, text, 1200), open(ROOT, text, 1400));
        let memory = run(&body);
        assert_error(&memory, 1200, 8, NO_ENTRY, &format!("stat-at `{text}`"));
        assert_error(&memory, 1400, 4, NO_ENTRY, &format!("open-at `{text}`"));
    }
}

#[test]
fn a_parent_escape_is_not_permitted() {
    let body = format!(
        "{} {} {}",
        stat_at(ROOT, "../etc/ssl", 1200),
        open(ROOT, "etc/ssl/certs", 1300),
        open(&opened(1300), "../../../../etc/hostname", 1400),
    );
    let memory = run(&body);
    assert_error(
        &memory,
        1200,
        8,
        NOT_PERMITTED,
        "a `..` past the root from the root",
    );
    assert_error(
        &memory,
        1400,
        4,
        NOT_PERMITTED,
        "a `..` past the root from a subdirectory",
    );
}

#[test]
fn a_path_through_the_file_is_not_a_directory() {
    let body = format!(
        "{} {} {}",
        stat_at(ROOT, "etc/ssl/certs/ca-certificates.crt/x", 1200),
        open(ROOT, BUNDLE_PATH, 1300),
        stat_at(&opened(1300), ".", 1400),
    );
    let memory = run(&body);
    assert_error(&memory, 1200, 8, NOT_DIRECTORY, "a lookup beneath the file");
    assert_error(
        &memory,
        1400,
        8,
        NOT_DIRECTORY,
        "a lookup relative to the file",
    );
}

#[test]
fn write_enabling_opens_of_the_file_are_read_only() {
    for (open_flags, descriptor_flags) in [(1, 0), (8, 0), (0, 2), (0, 32)] {
        let memory = run(&open_at(
            ROOT,
            BUNDLE_PATH,
            open_flags,
            descriptor_flags,
            1200,
        ));
        assert_error(
            &memory,
            1200,
            4,
            READ_ONLY,
            &format!(
                "open-at with open flags {open_flags} and descriptor flags {descriptor_flags}"
            ),
        );
    }
}

#[test]
fn the_file_opened_with_the_directory_flag_is_not_a_directory() {
    let body = format!(
        "{} {}",
        open_at(ROOT, BUNDLE_PATH, 2, 0, 1200),
        open_at(ROOT, "etc", 2, 0, 1300),
    );
    let memory = run(&body);
    assert_error(
        &memory,
        1200,
        4,
        NOT_DIRECTORY,
        "the file with the directory flag",
    );
    assert_ok(
        &memory,
        1300,
        "an implied directory with the directory flag",
    );
}

#[test]
fn a_stream_over_the_file_is_not_permitted() {
    let body = format!(
        "{} {} {}",
        open(ROOT, BUNDLE_PATH, 1200),
        call(
            Operation::ReadViaStream,
            &[&opened(1200), "(i64.const 0)", &ret(1300)]
        ),
        call(
            Operation::ReadViaStream,
            &[ROOT, "(i64.const 0)", &ret(1400)]
        ),
    );
    let memory = run(&body);
    assert_error(
        &memory,
        1300,
        4,
        NOT_PERMITTED,
        "read-via-stream on the file, whose blocking read would be blocking I/O",
    );
    assert_error(
        &memory,
        1400,
        4,
        IS_DIRECTORY,
        "read-via-stream on the root",
    );
}

#[test]
fn read_directory_on_the_file_is_not_a_directory() {
    let body = format!(
        "{} {}",
        open(ROOT, BUNDLE_PATH, 1200),
        call(Operation::ReadDirectory, &[&opened(1200), &ret(1300)]),
    );
    let memory = run(&body);
    assert_error(
        &memory,
        1300,
        4,
        NOT_DIRECTORY,
        "read-directory on the file",
    );
}

#[test]
fn mutations_of_the_file_are_read_only() {
    let file = opened(1200);
    let certs = opened(1300);
    let cases: [(&str, String, usize); 8] = [
        (
            "write",
            call(
                Operation::Write,
                &[
                    &file,
                    "(i32.const 2000)",
                    "(i32.const 1)",
                    "(i64.const 0)",
                    &ret(3000),
                ],
            ),
            8,
        ),
        (
            "set-size",
            call(Operation::SetSize, &[&file, "(i64.const 0)", &ret(3100)]),
            1,
        ),
        (
            "set-times",
            call(
                Operation::SetTimes,
                &[
                    &file,
                    "(i32.const 1)",
                    "(i64.const 0)",
                    "(i32.const 0)",
                    "(i32.const 1)",
                    "(i64.const 0)",
                    "(i32.const 0)",
                    &ret(3200),
                ],
            ),
            1,
        ),
        (
            "unlink-file-at",
            call(
                Operation::UnlinkFileAt,
                &[&certs, &path("ca-certificates.crt"), &ret(3300)],
            ),
            1,
        ),
        (
            "rename-at",
            call(
                Operation::RenameAt,
                &[
                    &certs,
                    &path("ca-certificates.crt"),
                    ROOT,
                    &path("etc/passwd"),
                    &ret(3400),
                ],
            ),
            1,
        ),
        (
            "write-via-stream",
            call(
                Operation::WriteViaStream,
                &[&file, "(i64.const 0)", &ret(3500)],
            ),
            4,
        ),
        (
            "append-via-stream",
            call(Operation::AppendViaStream, &[&file, &ret(3600)]),
            4,
        ),
        (
            "set-times-at",
            call(
                Operation::SetTimesAt,
                &[
                    &certs,
                    "(i32.const 0)",
                    &path("ca-certificates.crt"),
                    "(i32.const 1)",
                    "(i64.const 0)",
                    "(i32.const 0)",
                    "(i32.const 1)",
                    "(i64.const 0)",
                    "(i32.const 0)",
                    &ret(3700),
                ],
            ),
            1,
        ),
    ];
    let body = format!(
        "{} {} {}",
        open(ROOT, BUNDLE_PATH, 1200),
        open(ROOT, "etc/ssl/certs", 1300),
        cases
            .iter()
            .map(|(_, text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let memory = run(&body);
    for (index, (name, _, offset)) in cases.iter().enumerate() {
        assert_error(
            &memory,
            3000 + index * 100,
            *offset,
            READ_ONLY,
            &format!("{name} on the declared file"),
        );
    }
}

#[test]
fn readlink_of_the_declared_file_is_invalid() {
    let body = call(
        Operation::ReadlinkAt,
        &[ROOT, &path(BUNDLE_PATH), &ret(1200)],
    );
    let memory = run(&body);
    assert_error(&memory, 1200, 4, INVALID, "readlink-at of a regular file");
}

#[test]
fn an_invalid_descriptor_is_bad_descriptor_for_file_operations() {
    let body = format!(
        "{} {} {} {}",
        read("(i32.const 12345)", 10, 0, 1200),
        open(ROOT, BUNDLE_PATH, 1300),
        call(Operation::DirectoryEntry, &[&opened(1300), &ret(1400)]),
        call(
            Operation::Read,
            &[ROOT, "(i64.const 1)", "(i64.const 0)", &ret(1500)]
        ),
    );
    let memory = run(&body);
    assert_error(
        &memory,
        1200,
        4,
        BAD_DESCRIPTOR,
        "read on an invented descriptor",
    );
    assert_error(
        &memory,
        1400,
        4,
        BAD_DESCRIPTOR,
        "read-directory-entry on a descriptor that is not a stream",
    );
    assert_error(&memory, 1500, 4, IS_DIRECTORY, "read on the root directory");
}

#[test]
fn file_descriptors_and_directory_cursors_survive_capture_in_guest_memory_alone() {
    let engine = engine_with(declared());
    let park = format!(
        "{} {} {} {} (return (i64.const 1))",
        open(ROOT, BUNDLE_PATH, 1200),
        open(ROOT, "etc", 1300),
        call(Operation::ReadDirectory, &[&opened(1300), &ret(1400)]),
        call(Operation::DirectoryEntry, &[&opened(1400), &ret(1500)]),
    );
    let wake = format!(
        "{} {} {DONE}",
        read(&opened(1200), 4096, 0, 1600),
        call(Operation::DirectoryEntry, &[&opened(1400), &ret(1700)]),
    );
    let wasm = filesystem::lifecycle_module(&park, &wake, &strings());
    let template = engine.load_template(&wasm).expect("lifecycle template");
    let mut original = activate(&engine, &template);
    original.dispatch(&dispatch(), OccupancyBudget::default());
    assert!(
        original.is_parked(),
        "the guest parks with its descriptors open"
    );
    let image = original.capture_mutable_state().expect("state capture");
    let bytes = original.memory().to_vec();
    drop(original);
    let mut restored = activate(&engine, &template);
    restored
        .restore_mutable_state(&image)
        .expect("state restore");
    restored.memory().copy_from_slice(&bytes);
    let result = restored.wake(
        1,
        1,
        &isoplane_abi::result::ResultRecord::Ok(vec![]),
        OccupancyBudget::default(),
    );
    assert_eq!(
        result.completion,
        Completion::Done,
        "restored descriptors need no host resource rebinding"
    );
    let memory = restored.memory();
    assert_ok(memory, 1600, "a read through the restored file descriptor");
    assert_eq!(
        list(memory, 1604),
        BUNDLE,
        "the restored file descriptor still reads the declared bytes"
    );
    assert_eq!(
        (
            String::from_utf8_lossy(list(memory, 1712)).into_owned(),
            memory[1708]
        ),
        ("ssl".to_string(), DIRECTORY),
        "the restored directory stream continues from its cursor"
    );
}

#[test]
fn an_empty_declared_set_is_the_empty_root() {
    let engine = engine_with(PlatformFiles::new(vec![]).expect("an empty set is valid"));
    let body = format!(
        "{} {} {} {} {}",
        call(Operation::Stat, &[ROOT, RET]),
        call(Operation::ReadDirectory, &[ROOT, &ret(1300)]),
        call(Operation::DirectoryEntry, &[&opened(1300), &ret(1400)]),
        stat_at(ROOT, "etc", 1500),
        call(Operation::Hash, &[ROOT, &ret(1700)]),
    );
    let wasm = filesystem::module(&body, &strings());
    let memory = filesystem::run(&engine, &wasm);
    let mut stat = [0u8; 104];
    stat[8] = DIRECTORY;
    stat[16] = 1;
    assert_eq!(
        &memory[1100..1204],
        &stat,
        "the root stats as an empty directory"
    );
    assert_eq!(
        &memory[1400..1420],
        &[0; 20],
        "the root enumeration is EOF at once"
    );
    assert_error(&memory, 1500, 8, NO_ENTRY, "a path under an empty set");
    let mut hash = [0u8; 24];
    hash[8..12].copy_from_slice(&0x77534601u32.to_le_bytes());
    assert_eq!(
        &memory[1700..1724],
        &hash,
        "the root keeps its fixed metadata hash"
    );
    let undeclared = filesystem::run(&test_engine(), &wasm);
    assert_eq!(
        &memory[1100..1724],
        &undeclared[1100..1724],
        "an explicitly empty set answers byte for byte as an engine that declares nothing"
    );
}

/// The three timestamps of the `descriptor-stat` at `at`, as nanoseconds.
fn timestamps(memory: &[u8], at: usize) -> [Option<u64>; 3] {
    [32, 56, 80].map(|base| {
        (memory[at + base] == 1).then(|| {
            quad(memory, at + base + 8) * 1_000_000_000 + u64::from(word(memory, at + base + 16))
        })
    })
}

#[test]
fn a_directory_is_modified_when_its_newest_file_beneath_was() {
    let body = format!(
        "{} {} {} {} {}",
        stat_at(ROOT, "etc/ssl/certs", 1200),
        stat_at(ROOT, "etc", 1400),
        stat_at(ROOT, "usr", 1600),
        call(Operation::Stat, &[ROOT, &ret(1800)]),
        stat_at(ROOT, "etc/hostname", 2000),
    );
    let memory = run(&body);
    assert_eq!(
        timestamps(&memory, 1200),
        [Some(BUNDLE_MODIFIED_NS); 3],
        "the bundle's directory reports the bundle's modification time, so a rotation is visible there"
    );
    assert_eq!(
        timestamps(&memory, 1400),
        [Some(BUNDLE_MODIFIED_NS); 3],
        "an ancestor reports the newest file anywhere beneath it"
    );
    assert_eq!(
        timestamps(&memory, 1600),
        [Some(1); 3],
        "an unrelated subtree reports its own newest file"
    );
    assert_eq!(
        timestamps(&memory, 1800),
        [Some(BUNDLE_MODIFIED_NS); 3],
        "the root reports the newest file of the whole set"
    );
    assert_eq!(
        timestamps(&memory, 2000),
        [Some(0); 3],
        "a file keeps its own declared time even when a sibling is newer"
    );
}

#[test]
fn metadata_hashes_are_stable_across_engines_serving_the_same_set() {
    let body = format!(
        "{} {} {}",
        call(Operation::Hash, &[ROOT, &ret(1200)]),
        call(
            Operation::HashAt,
            &[ROOT, "(i32.const 0)", &path("etc/ssl/certs"), &ret(1300)]
        ),
        call(
            Operation::HashAt,
            &[ROOT, "(i32.const 0)", &path(BUNDLE_PATH), &ret(1400)]
        ),
    );
    let wasm = filesystem::module(&body, &strings());
    let first = filesystem::run(&engine_with(declared()), &wasm);
    let second = filesystem::run(&engine_with(declared()), &wasm);
    assert_eq!(
        &first[1200..1424],
        &second[1200..1424],
        "a store provider keyed on (ino, dev) must see the same identity from every engine serving the set"
    );
    assert_eq!(
        &first[1408..1424],
        &blake3::hash(b"/etc/ssl/certs/ca-certificates.crt").as_bytes()[..16],
        "a node's identity is a function of its path alone"
    );
}

#[path = "platform_files/application.rs"]
mod application;
