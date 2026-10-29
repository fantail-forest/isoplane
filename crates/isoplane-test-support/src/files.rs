//! File trees tests seed, copy and search.

use std::path::{Path, PathBuf};

/// Gives `destination` each file of `source` that `include` accepts, under
/// its own name, leaving names `destination` already holds alone: an entry
/// that is already a link to the source would be truncated by a copy onto
/// it. Unix hard-links and copies where the filesystem refuses a link.
/// Windows always copies, because a link shares the source's read-only
/// attribute and cleanup could not remove it.
pub fn link_files(source: &Path, destination: &Path, include: impl Fn(&Path) -> bool) {
    for entry in std::fs::read_dir(source)
        .unwrap_or_else(|error| panic!("list {}: {error}", source.display()))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("read {}: {error}", source.display()))
            .path();
        let target = destination.join(path.file_name().expect("an entry has a name"));
        if !include(&path) || target.exists() {
            continue;
        }
        link_or_copy(&path, &target)
            .unwrap_or_else(|error| panic!("link {}: {error}", target.display()));
    }
}

#[cfg(unix)]
fn link_or_copy(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::hard_link(source, target).or_else(|_| std::fs::copy(source, target).map(drop))
}

#[cfg(windows)]
fn link_or_copy(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::copy(source, target).map(drop)
}

/// Copies the tree under `source` to `destination`, keeping each file's
/// modification time so that a build sees copied outputs as current.
pub fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination)
        .unwrap_or_else(|error| panic!("create {}: {error}", destination.display()));
    for entry in std::fs::read_dir(source)
        .unwrap_or_else(|error| panic!("read {}: {error}", source.display()))
    {
        let entry = entry.unwrap_or_else(|error| panic!("list {}: {error}", source.display()));
        let target = destination.join(entry.file_name());
        let metadata = entry
            .metadata()
            .unwrap_or_else(|error| panic!("inspect {}: {error}", entry.path().display()));
        if metadata.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target)
                .unwrap_or_else(|error| panic!("copy {}: {error}", entry.path().display()));
            std::fs::File::options()
                .write(true)
                .open(&target)
                .and_then(|file| file.set_modified(metadata.modified()?))
                .unwrap_or_else(|error| panic!("date {}: {error}", target.display()));
        }
    }
}

/// Every directory below `root`, at any depth, in no particular order.
pub fn directories_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("list {}: {error}", directory.display()))
        {
            let path = entry
                .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
                .path();
            if path.is_dir() {
                found.push(path.clone());
                pending.push(path);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{copy_tree, directories_under, link_files};
    use std::time::{Duration, SystemTime};

    #[test]
    fn linked_files_match_the_filter_and_leave_existing_names_alone() {
        let directory = tempfile::tempdir().expect("directory");
        let source = directory.path().join("source");
        let destination = directory.path().join("destination");
        std::fs::create_dir_all(&source).expect("source");
        std::fs::create_dir_all(&destination).expect("destination");
        std::fs::write(source.join("a.cwasm"), b"a").expect("write");
        std::fs::write(source.join("b.meta"), b"b").expect("write");
        std::fs::write(source.join("c.txt"), b"c").expect("write");
        std::fs::write(destination.join("b.meta"), b"kept").expect("write");
        link_files(&source, &destination, |path| {
            path.extension().is_some_and(|extension| extension != "txt")
        });
        assert_eq!(
            std::fs::read(destination.join("a.cwasm")).expect("linked"),
            b"a"
        );
        assert_eq!(
            std::fs::read(destination.join("b.meta")).expect("kept"),
            b"kept",
            "an existing name is left alone"
        );
        assert!(
            !destination.join("c.txt").exists(),
            "a file the filter rejects is not linked"
        );
    }

    #[test]
    fn a_copied_tree_keeps_its_modification_times() {
        let directory = tempfile::tempdir().expect("directory");
        let source = directory.path().join("source");
        std::fs::create_dir_all(source.join("nested")).expect("source");
        let file = source.join("nested/file");
        std::fs::write(&file, b"dated").expect("write");
        let dated = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        std::fs::File::options()
            .write(true)
            .open(&file)
            .and_then(|file| file.set_modified(dated))
            .expect("date the source");
        let destination = directory.path().join("destination");
        copy_tree(&source, &destination);
        let copied = destination.join("nested/file");
        assert_eq!(std::fs::read(&copied).expect("copied"), b"dated");
        assert_eq!(
            std::fs::metadata(&copied)
                .expect("copied")
                .modified()
                .expect("mtime"),
            dated,
            "the copy keeps the source's modification time"
        );
    }

    #[test]
    fn directories_under_a_root_include_every_depth_but_not_files() {
        let directory = tempfile::tempdir().expect("directory");
        let root = directory.path();
        std::fs::create_dir_all(root.join("a/b/c")).expect("tree");
        std::fs::create_dir_all(root.join("d")).expect("tree");
        std::fs::write(root.join("a/file"), b"").expect("file");
        let mut found = directories_under(root);
        found.sort();
        assert_eq!(
            found,
            [
                root.join("a"),
                root.join("a/b"),
                root.join("a/b/c"),
                root.join("d")
            ]
        );
    }
}
