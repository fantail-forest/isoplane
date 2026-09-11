#![forbid(unsafe_code)]
//! Empty-filesystem conformance through the engine's real validator and linker.
mod common;
#[path = "common/filesystem.rs"]
mod filesystem;
use common::{activate, guest, test_engine, DONE};
use filesystem::{call, dispatch, RET, ROOT};
use wisp_abi::filesystem::{Operation, IMPORTS};
use wisp_abi::imports::Ty;
use wisp_engine::{Completion, CpuBudget, InstanceOps};

/// The paths the tests below name, at the addresses they use.
const STRINGS: &[(u32, &str)] = &[
    (2000, "."),
    (2010, "missing"),
    (2030, "../etc/passwd"),
    (2050, "/etc/passwd"),
    (2070, "././"),
];

fn module(body: &str) -> Vec<u8> {
    filesystem::module(body, STRINGS)
}

fn lifecycle_module(body: &str, wake: &str) -> Vec<u8> {
    filesystem::lifecycle_module(body, wake, STRINGS)
}

fn run(body: &str) -> Vec<u8> {
    filesystem::run(&test_engine(), &module(body))
}

#[test]
fn root_metadata_and_enumeration_describe_an_empty_directory() {
    let body = format!(
        "{} {} {}",
        call(Operation::Stat, &[ROOT, RET]),
        call(Operation::ReadDirectory, &[ROOT, "(i32.const 1300)"]),
        call(
            Operation::DirectoryEntry,
            &["(i32.load (i32.const 1304))", "(i32.const 1400)"]
        )
    );
    let memory = run(&body);
    assert_eq!(
        &memory[1100..1109],
        &[0, 0, 0, 0, 0, 0, 0, 0, 3],
        "the only filesystem object is a directory"
    );
    assert_eq!(
        &memory[1400..1408],
        &[0; 8],
        "a root enumeration must return EOF without host entries"
    );
}

#[test]
fn missing_absolute_and_traversal_paths_cannot_reach_host_files() {
    for (pointer, length, expected) in [(2010, 7, 20), (2030, 13, 31), (2050, 11, 31)] {
        let memory = run(&call(
            Operation::StatAt,
            &[
                ROOT,
                "(i32.const 1)",
                &format!("(i32.const {pointer})"),
                &format!("(i32.const {length})"),
                RET,
            ],
        ));
        assert_eq!(
            (memory[1100], memory[1108]),
            (1, expected),
            "missing and escaping paths must be denied within the synthetic root"
        );
    }
}

#[test]
fn every_mutating_operation_refuses_a_valid_directory() {
    for operation in [
        Operation::SetSize,
        Operation::SetTimes,
        Operation::Write,
        Operation::WriteViaStream,
        Operation::AppendViaStream,
        Operation::CreateDirectoryAt,
        Operation::RemoveDirectoryAt,
        Operation::RenameAt,
        Operation::SymlinkAt,
        Operation::LinkAt,
        Operation::UnlinkFileAt,
    ] {
        let definition = &IMPORTS[operation as usize];
        let signature = definition.pal.sig.expect("filesystem signature");
        let mut arguments: Vec<String> = signature
            .params
            .iter()
            .map(|ty| match ty {
                Ty::I32 => "(i32.const 0)".into(),
                Ty::I64 => "(i64.const 0)".into(),
            })
            .collect();
        arguments[0] = ROOT.into();
        let last = arguments.len() - 1;
        arguments[last] = RET.into();
        if operation == Operation::RenameAt {
            arguments[3] = ROOT.into();
        }
        if operation == Operation::LinkAt {
            arguments[4] = ROOT.into();
        }
        let refs: Vec<&str> = arguments.iter().map(String::as_str).collect();
        let memory = run(&call(operation, &refs));
        let offset = match operation {
            Operation::Write => 8,
            Operation::WriteViaStream | Operation::AppendViaStream => 4,
            _ => 1,
        };
        assert_eq!(
            (memory[1100], memory[1100 + offset]),
            (1, 33),
            "{operation:?} must return read-only before path lookup"
        );
    }
}

#[test]
fn write_open_flags_cannot_obtain_a_mutable_descriptor() {
    for (open_flags, descriptor_flags) in [(1, 0), (8, 0), (0, 2), (0, 32)] {
        let memory = run(&call(
            Operation::OpenAt,
            &[
                ROOT,
                "(i32.const 1)",
                "(i32.const 2000)",
                "(i32.const 1)",
                &format!("(i32.const {open_flags})"),
                &format!("(i32.const {descriptor_flags})"),
                RET,
            ],
        ));
        assert_eq!(
            (memory[1100], memory[1104]),
            (1, 33),
            "each write-enabling open flag must return read-only"
        );
    }
}

#[test]
fn inherited_invented_and_closed_descriptors_are_invalid() {
    for descriptor in [0, 1, 2, 3, u32::MAX] {
        let memory = run(&call(
            Operation::Stat,
            &[&format!("(i32.const {descriptor})"), RET],
        ));
        assert_eq!(
            (memory[1100], memory[1108]),
            (1, 3),
            "descriptor {descriptor} must not address a host file"
        );
    }
    let memory = run(&format!(
        "{} {}",
        call(Operation::DropDescriptor, &[ROOT]),
        call(Operation::Stat, &[ROOT, RET])
    ));
    assert_eq!(
        (memory[1100], memory[1108]),
        (1, 3),
        "a closed descriptor must fail with bad-descriptor"
    );
}

#[test]
fn read_only_sync_succeeds_without_creating_a_write_capability() {
    let memory = run(&call(Operation::Sync, &[ROOT, RET]));
    assert_eq!(
        memory[1100], 0,
        "WASI sync on a read-only descriptor is a successful no-op"
    );
}

#[test]
fn filesystem_handles_survive_capture_in_guest_memory_alone() {
    let engine = test_engine();
    let create_stream = call(Operation::ReadDirectory, &[ROOT, "(i32.const 1300)"]);
    let wake = format!(
        "{} {} {DONE}",
        call(Operation::Stat, &[ROOT, RET]),
        call(
            Operation::DirectoryEntry,
            &["(i32.load (i32.const 1304))", "(i32.const 1400)"]
        )
    );
    let wasm = lifecycle_module(&format!("{create_stream} (return (i64.const 1))"), &wake);
    let template = engine
        .load_template(&wasm)
        .expect("filesystem lifecycle template");
    let mut original = activate(&engine, &template);
    original.dispatch(&dispatch(), CpuBudget::http());
    assert!(
        original.is_parked(),
        "filesystem handles must be captured only on an empty stack"
    );
    let image = original
        .capture_mutable_state()
        .expect("filesystem state capture");
    let bytes = original.memory().to_vec();
    drop(original);
    let mut restored = activate(&engine, &template);
    restored
        .restore_mutable_state(&image)
        .expect("filesystem state restore");
    restored.memory().copy_from_slice(&bytes);
    let result = restored.wake(
        1,
        &wisp_abi::result::ResultRecord::Ok(vec![]),
        CpuBudget::http(),
    );
    assert_eq!(
        result.completion,
        Completion::Done,
        "restored descriptors must need no host resource rebinding"
    );
    assert_eq!(
        restored.memory()[1108],
        3,
        "the restored descriptor must still describe the root"
    );
    assert_eq!(
        &restored.memory()[1400..1408],
        &[0; 8],
        "the restored iterator must still report EOF"
    );
}

#[test]
fn a_filesystem_import_with_the_wrong_signature_is_rejected() {
    let invalid = guest(
        4,
        r#"(import "wasi:filesystem/types@0.2.0" "[method]descriptor.stat" (func (param i32)))"#,
        "",
        DONE,
        DONE,
    );
    assert!(
        test_engine().load_template(&invalid).is_err(),
        "an executing filesystem adapter must reject a mismatched canonical signature"
    );
}

#[test]
fn malformed_filesystem_packets_trap_inside_the_guest() {
    for (operation, pointer, count) in [(u32::MAX, 0, 0), (0, 0, 0), (0, u32::MAX, 1)] {
        let wasm = guest(4, r#"(import "wisp" "filesystem_call" (func $filesystem (param i32 i32 i32)))"#, "", &format!("(call $filesystem (i32.const {operation}) (i32.const {pointer}) (i32.const {count})) {DONE}"), DONE);
        let engine = test_engine();
        let template = engine
            .load_template(&wasm)
            .expect("packet import signature is admitted");
        let mut instance = activate(&engine, &template);
        let result = instance.dispatch(&dispatch(), CpuBudget::http());
        assert!(
            result.trap.is_some(),
            "malformed filesystem packet must fail the dispatch without a host panic"
        );
    }
}

#[test]
fn the_roots_parent_is_the_same_root_without_authorizing_traversal() {
    let memory = run(&call(
        Operation::StatAt,
        &[
            ROOT,
            "(i32.const 0)",
            "(i32.const 2030)",
            "(i32.const 2)",
            RET,
        ],
    ));
    assert_eq!((memory[1100],memory[1108]), (0,3), "libc's synthetic parent entry must stat as the root directory so managed enumeration can omit it");
}

#[test]
fn symbolic_link_reads_cannot_resolve_a_host_target() {
    for (pointer, length, code) in [(2010, 7, 20), (2050, 11, 31), (2000, 1, 12)] {
        let memory = run(&call(
            Operation::ReadlinkAt,
            &[
                ROOT,
                &format!("(i32.const {pointer})"),
                &format!("(i32.const {length})"),
                RET,
            ],
        ));
        assert_eq!(
            (memory[1100], memory[1104]),
            (1, code),
            "the empty root contains no symlink and cannot resolve an absolute host path"
        );
    }
}
