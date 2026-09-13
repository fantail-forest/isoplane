//! Destructive integrity control restricted to the explicitly supplied disposable loop image.

use std::path::Path;
use std::process::Command;

/// Corrupts one data block of a test-owned artifact, then evicts its file-cache pages.
pub fn corrupt_first_block(path: &Path) {
    corrupt_block(path, 0);
}

/// Corrupts the selected logical block after confirming its owned loop backing.
pub fn corrupt_block(path: &Path, logical_block: u64) {
    let image = std::env::var_os("WISP_VERITY_TEST_IMAGE")
        .expect("corruption tests require WISP_VERITY_TEST_IMAGE naming the disposable loop image");
    let root = std::env::var_os("WISP_VERITY_TEST_ROOT").expect("verity root");
    assert!(
        path.starts_with(&root),
        "corruption target must stay inside the owned fixture root"
    );
    let output = Command::new("findmnt")
        .args(["--noheadings", "--output", "SOURCE", "--target"])
        .arg(path)
        .output()
        .expect("findmnt");
    assert!(
        output.status.success(),
        "fixture filesystem must be identifiable"
    );
    let device = String::from_utf8(output.stdout).expect("device name");
    let device = device.trim();
    assert!(
        device.starts_with("/dev/loop"),
        "corruption is restricted to a disposable loop device"
    );
    let output = Command::new("losetup")
        .args(["--noheadings", "--output", "BACK-FILE", device])
        .output()
        .expect("losetup");
    assert!(
        output.status.success(),
        "loop backing image must be identifiable"
    );
    assert_eq!(
        Path::new(
            String::from_utf8(output.stdout)
                .expect("backing path")
                .trim()
        ),
        Path::new(&image),
        "corruption must target only the explicitly owned loop image"
    );
    let output = Command::new("filefrag")
        .args(["-v", "-b4096"])
        .arg(path)
        .output()
        .expect("filefrag");
    assert!(
        output.status.success(),
        "artifact data extent must be identifiable"
    );
    let extents = String::from_utf8(output.stdout).expect("extent output");
    let physical = extents
        .lines()
        .filter_map(|line| {
            let columns: Vec<_> = line.split(':').collect();
            if columns.len() < 4 {
                return None;
            }
            let logical: Vec<_> = columns[1].trim().split("..").collect();
            if logical.len() != 2 {
                return None;
            }
            let start = logical[0].trim().parse::<u64>().ok()?;
            let end = logical[1].trim().parse::<u64>().ok()?;
            if !(start..=end).contains(&logical_block) {
                return None;
            }
            let physical = columns[2]
                .trim()
                .split("..")
                .next()?
                .trim()
                .parse::<u64>()
                .ok()?;
            Some(physical + logical_block - start)
        })
        .next()
        .expect("data extent covering selected logical block");
    let status = Command::new("sudo")
        .args([
            "-n",
            "dd",
            "if=/dev/zero",
            &format!("of={device}"),
            "bs=4096",
            &format!("seek={physical}"),
            "count=1",
            "conv=notrunc",
            "oflag=direct",
            "status=none",
        ])
        .status()
        .expect("write owned corruption control");
    assert!(
        status.success(),
        "owned artifact backing block must actually be corrupted"
    );
    let file = std::fs::File::open(path).expect("hold corrupted artifact inode");
    use std::os::fd::AsRawFd;
    // SAFETY: the live descriptor names only this fixture; DONTNEED is file-cache advice.
    let result = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    assert_eq!(
        result, 0,
        "corrupted fixture pages must be eligible for eviction"
    );
}

/// Evicts the selected worker's file-backed executable pages so the next instruction fetch reads storage.
pub fn evict_executable_mapping(process_id: u32, path: &Path, offset_bytes: u64) {
    let script = r#"
import ctypes, json, os, struct, sys
pid, path = int(sys.argv[1]), sys.argv[2]
offset, advice = int(sys.argv[3]), int(sys.argv[4])
class Iovec(ctypes.Structure):
    _fields_ = [('base', ctypes.c_void_p), ('length', ctypes.c_size_t)]
libc = ctypes.CDLL(None, use_errno=True)
libc.process_madvise.argtypes = [ctypes.c_int, ctypes.POINTER(Iovec), ctypes.c_size_t, ctypes.c_int, ctypes.c_uint]
libc.process_madvise.restype = ctypes.c_ssize_t
fd = os.pidfd_open(pid)
matched = False
try:
    for line in open(f'/proc/{pid}/maps'):
        fields = line.split()
        if len(fields) >= 6 and fields[-1] == path and 'x' in fields[1]:
            start, end = [int(value,16) for value in fields[0].split('-')]
            virtual = start + offset - int(fields[2], 16)
            if not start <= virtual < end:
                continue
            def resident():
                with open(f'/proc/{pid}/pagemap', 'rb') as pages:
                    pages.seek((virtual // os.sysconf('SC_PAGE_SIZE')) * 8)
                    return bool(struct.unpack('Q', pages.read(8))[0] & (1 << 63))
            before = resident()
            region = Iovec(start, end-start)
            result = libc.process_madvise(fd, ctypes.byref(region), 1, advice, 0)
            if result != end-start:
                raise OSError(ctypes.get_errno(), 'process_madvise PAGEOUT failed')
            after = resident()
            print(json.dumps({'event':'native_mapping_eviction','pid':pid,'file':path,'offset_bytes':offset,'advised_bytes':result,'resident_before':before,'resident_after':after}), flush=True)
            if after:
                raise RuntimeError('target executable page remained resident after PAGEOUT')
            matched = True
    if not matched:
        raise RuntimeError('owned worker has no executable mapping of its native artifact')
finally:
    os.close(fd)
"#;
    let status = Command::new("sudo")
        .args(["-n", "python3", "-c", script, &process_id.to_string()])
        .arg(path)
        .arg(offset_bytes.to_string())
        .arg(libc::MADV_PAGEOUT.to_string())
        .status()
        .expect("evict owned executable mapping");
    assert!(
        status.success(),
        "the owned worker's executable pages must be evicted before testing a backing fault"
    );
}
