//! The host-declared set of platform-managed files a guest can read through
//! the PAL filesystem. The set is small and fixed for an engine's lifetime;
//! the directories its paths imply exist implicitly, and nothing outside it
//! exists at all.

use std::collections::BTreeMap;
use std::sync::Arc;

/// One read-only regular file served from host memory.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlatformFile {
    /// Absolute, normalised path: a leading `/`, no empty, `.` or `..`
    /// component.
    pub path: String,
    #[serde(with = "file_bytes")]
    pub bytes: Arc<[u8]>,
    /// Reported as the file's access, modification and status-change time.
    pub modified_ns: u64,
}

/// Why a declared set is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlatformFilesError {
    #[error("platform file path `{path}` is not absolute")]
    RelativePath { path: String },
    #[error("platform file path `{path}` has an empty component")]
    EmptyComponent { path: String },
    #[error("platform file path `{path}` has a `.` or `..` component")]
    DotComponent { path: String },
    #[error("platform file path `{path}` is declared twice")]
    DuplicatePath { path: String },
    #[error("platform file path `{path}` is a directory on the way to `{file}`")]
    DirectoryConflict { path: String, file: String },
}

/// A position checked against a [`PlatformFiles`] tree. Consumers interpret
/// the index in that tree; it carries no identity for its originating set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Node(u32);

const FILE_NODE: u32 = 1 << 31;

/// What a node is, for the operations that answer differently per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Root,
    Directory,
    File,
}

impl Node {
    pub(crate) const ROOT: Node = Node(0);

    fn directory(index: usize) -> Node {
        Node(index as u32 + 1)
    }

    fn file(index: usize) -> Node {
        Node(FILE_NODE | index as u32)
    }

    /// The record word: `0` is the root, directories count from `1`, files
    /// carry the top bit.
    pub(crate) fn word(self) -> u32 {
        self.0
    }

    pub(crate) fn kind(self) -> NodeKind {
        if self.0 == 0 {
            NodeKind::Root
        } else if self.0 & FILE_NODE != 0 {
            NodeKind::File
        } else {
            NodeKind::Directory
        }
    }

    fn index(self) -> usize {
        match self.kind() {
            NodeKind::Root => 0,
            NodeKind::Directory => (self.0 - 1) as usize,
            NodeKind::File => (self.0 & !FILE_NODE) as usize,
        }
    }
}

/// Why a path lookup failed, in the tree's own terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LookupError {
    NoEntry,
    NotDirectory,
    Escape,
}

#[derive(Debug)]
pub(crate) struct Child {
    pub name: String,
    pub node: Node,
}

#[derive(Debug)]
struct Directory {
    path: String,
    parent: Node,
    children: Vec<Child>,
    /// The latest `modified_ns` of any file beneath, so a consumer that
    /// watches a directory's modification time sees a file rotate.
    modified_ns: u64,
}

#[derive(Debug, Default)]
struct Tree {
    files: Vec<PlatformFile>,
    /// Parallel to `files`.
    file_parents: Vec<Node>,
    directories: Vec<Directory>,
    root: Vec<Child>,
    /// The latest `modified_ns` of any file; `None` for the empty set.
    root_modified_ns: Option<u64>,
}

/// The declared set, cheap to clone and share across every store of an
/// engine.
#[derive(Debug, Clone, Default)]
pub struct PlatformFiles(Arc<Tree>);

impl serde::Serialize for PlatformFiles {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.files().serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for PlatformFiles {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let files = Vec::<PlatformFile>::deserialize(deserializer)?;
        Self::new(files).map_err(serde::de::Error::custom)
    }
}

impl PlatformFiles {
    /// Validate and index the set. Fails on a relative path, an empty, `.`
    /// or `..` component, a path declared twice, or a file whose path is a
    /// directory on the way to another file.
    pub fn new(files: Vec<PlatformFile>) -> Result<Self, PlatformFilesError> {
        let mut files = files;
        for file in &files {
            check_path(&file.path)?;
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        if let Some(pair) = files.windows(2).find(|pair| pair[0].path == pair[1].path) {
            return Err(PlatformFilesError::DuplicatePath {
                path: pair[0].path.clone(),
            });
        }
        // Directory path -> the first file that implied it.
        let mut implied: BTreeMap<String, String> = BTreeMap::new();
        for file in &files {
            for ancestor in ancestors(&file.path) {
                implied
                    .entry(ancestor.to_string())
                    .or_insert_with(|| file.path.clone());
            }
        }
        for file in &files {
            if let Some(below) = implied.get(&file.path) {
                return Err(PlatformFilesError::DirectoryConflict {
                    path: file.path.clone(),
                    file: below.clone(),
                });
            }
        }
        let directory_index: BTreeMap<&str, usize> = implied
            .keys()
            .enumerate()
            .map(|(index, path)| (path.as_str(), index))
            .collect();
        // Every ancestor of a declared path is in `implied`, so the only
        // parent path absent from the index is the root itself.
        let parent_of = |path: &str| {
            directory_index
                .get(parent_path(path))
                .map_or(Node::ROOT, |index| Node::directory(*index))
        };
        let mut directories: Vec<Directory> = implied
            .keys()
            .map(|path| Directory {
                path: path.clone(),
                parent: parent_of(path),
                children: Vec::new(),
                modified_ns: 0,
            })
            .collect();
        for file in &files {
            for ancestor in ancestors(&file.path) {
                if let Some(index) = directory_index.get(ancestor) {
                    let directory = &mut directories[*index];
                    directory.modified_ns = directory.modified_ns.max(file.modified_ns);
                }
            }
        }
        let root_modified_ns = files.iter().map(|file| file.modified_ns).max();
        let mut root = Vec::new();
        let mut attach = |path: &str, node: Node| {
            let child = Child {
                name: base_name(path).to_string(),
                node,
            };
            match directory_index.get(parent_path(path)) {
                Some(index) => directories[*index].children.push(child),
                None => root.push(child),
            }
        };
        for (index, directory) in implied.keys().enumerate() {
            attach(directory, Node::directory(index));
        }
        for (index, file) in files.iter().enumerate() {
            attach(&file.path, Node::file(index));
        }
        root.sort_by(|a, b| a.name.cmp(&b.name));
        for directory in &mut directories {
            directory.children.sort_by(|a, b| a.name.cmp(&b.name));
        }
        let file_parents = files.iter().map(|file| parent_of(&file.path)).collect();
        Ok(Self(Arc::new(Tree {
            files,
            file_parents,
            directories,
            root,
            root_modified_ns,
        })))
    }

    pub fn is_empty(&self) -> bool {
        self.0.files.is_empty()
    }

    /// The declared files in path order.
    pub fn files(&self) -> &[PlatformFile] {
        &self.0.files
    }

    /// The node a record word names, or `None` for a word outside this set.
    pub(crate) fn decode(&self, word: u32) -> Option<Node> {
        let node = Node(word);
        let count = match node.kind() {
            NodeKind::Root => return Some(node),
            NodeKind::Directory => self.0.directories.len(),
            NodeKind::File => self.0.files.len(),
        };
        (node.index() < count).then_some(node)
    }

    pub(crate) fn file(&self, node: Node) -> Option<&PlatformFile> {
        match node.kind() {
            NodeKind::File => self.0.files.get(node.index()),
            NodeKind::Root | NodeKind::Directory => None,
        }
    }

    /// A file's declared modification time; for a directory, the latest of
    /// the files beneath it; `None` only for the root of the empty set.
    pub(crate) fn modified_ns(&self, node: Node) -> Option<u64> {
        match node.kind() {
            NodeKind::Root => self.0.root_modified_ns,
            NodeKind::Directory => self
                .0
                .directories
                .get(node.index())
                .map(|directory| directory.modified_ns),
            NodeKind::File => self.0.files.get(node.index()).map(|file| file.modified_ns),
        }
    }

    /// A directory's children in name order; `None` for a file.
    pub(crate) fn children(&self, node: Node) -> Option<&[Child]> {
        match node.kind() {
            NodeKind::Root => Some(&self.0.root),
            NodeKind::Directory => self
                .0
                .directories
                .get(node.index())
                .map(|directory| directory.children.as_slice()),
            NodeKind::File => None,
        }
    }

    /// The absolute path of a node; the root is `/`.
    pub(crate) fn path(&self, node: Node) -> &str {
        match node.kind() {
            NodeKind::Root => "/",
            NodeKind::Directory => self
                .0
                .directories
                .get(node.index())
                .map_or("/", |directory| directory.path.as_str()),
            NodeKind::File => self
                .0
                .files
                .get(node.index())
                .map_or("/", |file| file.path.as_str()),
        }
    }

    fn parent(&self, node: Node) -> Option<Node> {
        match node.kind() {
            NodeKind::Root => None,
            NodeKind::Directory => self
                .0
                .directories
                .get(node.index())
                .map(|directory| directory.parent),
            NodeKind::File => self.0.file_parents.get(node.index()).copied(),
        }
    }

    /// Resolve a guest-relative path from `from`, one component at a time.
    /// `.` and empty components stay put; `..` climbs, and at the root it
    /// stays there only when the whole path is made of such components
    /// (libc's synthetic parent entry), otherwise it is an escape.
    pub(crate) fn lookup(&self, from: Node, path: &str) -> Result<Node, LookupError> {
        let named = path.split('/').any(|part| !is_synthetic(part));
        let mut current = from;
        for part in path.split('/') {
            if current.kind() == NodeKind::File {
                return Err(LookupError::NotDirectory);
            }
            match part {
                "" | "." => {}
                ".." => match self.parent(current) {
                    Some(parent) => current = parent,
                    None if named => return Err(LookupError::Escape),
                    None => {}
                },
                name => {
                    current = self
                        .children(current)
                        .ok_or(LookupError::NotDirectory)?
                        .iter()
                        .find(|child| child.name == name)
                        .map(|child| child.node)
                        .ok_or(LookupError::NoEntry)?;
                }
            }
        }
        Ok(current)
    }
}

fn is_synthetic(part: &str) -> bool {
    part.is_empty() || part == "." || part == ".."
}

fn check_path(path: &str) -> Result<(), PlatformFilesError> {
    let Some(rest) = path.strip_prefix('/') else {
        return Err(PlatformFilesError::RelativePath { path: path.into() });
    };
    for part in rest.split('/') {
        if part.is_empty() {
            return Err(PlatformFilesError::EmptyComponent { path: path.into() });
        }
        if part == "." || part == ".." {
            return Err(PlatformFilesError::DotComponent { path: path.into() });
        }
    }
    Ok(())
}

/// The directories between the root and `path`, shortest first, excluding
/// the root and `path` itself.
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/')
        .skip(1)
        .map(move |(index, _)| &path[..index])
}

fn parent_path(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(index) => &path[..index],
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> PlatformFile {
        modified(path, 0)
    }

    fn modified(path: &str, modified_ns: u64) -> PlatformFile {
        PlatformFile {
            path: path.into(),
            bytes: Arc::from(&b"x"[..]),
            modified_ns,
        }
    }

    fn bundle() -> PlatformFiles {
        PlatformFiles::new(vec![
            modified("/etc/ssl/certs/ca-certificates.crt", 500),
            modified("/etc/hostname", 700),
            modified("/usr/share/zoneinfo/UTC", 300),
        ])
        .expect("a well-formed set")
    }

    #[test]
    fn a_directory_is_modified_when_its_newest_file_beneath_was() {
        let files = bundle();
        let at = |path: &str| {
            let node = files.lookup(Node::ROOT, path).expect(path);
            files.modified_ns(node)
        };
        assert_eq!(
            at("etc/ssl/certs"),
            Some(500),
            "a leaf directory carries its file's time"
        );
        assert_eq!(
            at("etc/ssl"),
            Some(500),
            "an intermediate directory carries the newest time beneath it"
        );
        assert_eq!(
            at("etc"),
            Some(700),
            "a directory with a newer sibling file reports that file's time"
        );
        assert_eq!(
            at("usr"),
            Some(300),
            "an unrelated subtree keeps its own time"
        );
        assert_eq!(
            files.modified_ns(Node::ROOT),
            Some(700),
            "the root reports the newest file anywhere"
        );
        assert_eq!(
            PlatformFiles::default().modified_ns(Node::ROOT),
            None,
            "the root of the empty set has no file to take a time from"
        );
    }

    #[test]
    fn a_relative_path_is_refused() {
        assert_eq!(
            PlatformFiles::new(vec![file("etc/hostname")]).err(),
            Some(PlatformFilesError::RelativePath {
                path: "etc/hostname".into()
            }),
            "a declared path must be absolute so it resolves from the preopened root"
        );
    }

    #[test]
    fn an_empty_component_is_refused() {
        for path in ["/etc//hostname", "/etc/hostname/"] {
            assert_eq!(
                PlatformFiles::new(vec![file(path)]).err(),
                Some(PlatformFilesError::EmptyComponent { path: path.into() }),
                "an unnormalised path would never match a guest lookup"
            );
        }
    }

    #[test]
    fn a_dot_component_is_refused() {
        for path in ["/etc/./hostname", "/etc/../hostname"] {
            assert_eq!(
                PlatformFiles::new(vec![file(path)]).err(),
                Some(PlatformFilesError::DotComponent { path: path.into() }),
                "a declared path must be normalised so the tree has one name per node"
            );
        }
    }

    #[test]
    fn a_duplicate_path_is_refused() {
        assert_eq!(
            PlatformFiles::new(vec![file("/etc/hostname"), file("/etc/hostname")]).err(),
            Some(PlatformFilesError::DuplicatePath {
                path: "/etc/hostname".into()
            }),
            "two files cannot share one path"
        );
    }

    #[test]
    fn a_file_on_the_way_to_another_file_is_refused() {
        assert_eq!(
            PlatformFiles::new(vec![
                file("/etc"),
                file("/etc-x"),
                file("/etc/ssl/certs/ca-certificates.crt")
            ])
            .err(),
            Some(PlatformFilesError::DirectoryConflict {
                path: "/etc".into(),
                file: "/etc/ssl/certs/ca-certificates.crt".into()
            }),
            "a path cannot be both a file and an implied directory"
        );
    }

    #[test]
    fn the_empty_set_has_only_the_root() {
        let files = PlatformFiles::default();
        assert!(files.is_empty(), "the default set declares nothing");
        assert_eq!(
            files.children(Node::ROOT).map(<[Child]>::len),
            Some(0),
            "the root of an empty set has no entries"
        );
        assert_eq!(
            files.lookup(Node::ROOT, "etc"),
            Err(LookupError::NoEntry),
            "nothing is declared, so nothing resolves"
        );
    }

    #[test]
    fn implied_directories_are_listed_in_name_order() {
        let files = bundle();
        let names = |node| -> Vec<String> {
            files
                .children(node)
                .expect("a directory")
                .iter()
                .map(|child| child.name.clone())
                .collect()
        };
        assert_eq!(
            names(Node::ROOT),
            ["etc", "usr"],
            "root lists its implied directories"
        );
        let etc = files.lookup(Node::ROOT, "etc").expect("etc exists");
        assert_eq!(
            names(etc),
            ["hostname", "ssl"],
            "a directory lists files and directories together, by name"
        );
    }

    #[test]
    fn a_path_resolves_stepwise_and_at_once_to_the_same_file() {
        let files = bundle();
        let at_once = files
            .lookup(Node::ROOT, "etc/ssl/certs/ca-certificates.crt")
            .expect("the file resolves in one lookup");
        let etc = files.lookup(Node::ROOT, "etc").expect("etc");
        let ssl = files.lookup(etc, "ssl").expect("ssl");
        let certs = files.lookup(ssl, "certs").expect("certs");
        let stepwise = files
            .lookup(certs, "ca-certificates.crt")
            .expect("the file resolves from its directory");
        assert_eq!(at_once, stepwise, "both routes name the same node");
        assert_eq!(at_once.kind(), NodeKind::File, "the leaf is a file");
        assert_eq!(
            files.path(at_once),
            "/etc/ssl/certs/ca-certificates.crt",
            "the node carries its absolute path"
        );
    }

    #[test]
    fn dot_dot_climbs_inside_the_tree_but_never_out_of_it() {
        let files = bundle();
        let certs = files.lookup(Node::ROOT, "etc/ssl/certs").expect("certs");
        assert_eq!(
            files.lookup(certs, "../../hostname"),
            files.lookup(Node::ROOT, "etc/hostname"),
            "a parent step inside the tree resolves like the absolute path"
        );
        assert_eq!(
            files.lookup(Node::ROOT, ".."),
            Ok(Node::ROOT),
            "libc's synthetic parent of the root is the root"
        );
        assert_eq!(
            files.lookup(Node::ROOT, "../etc/hostname"),
            Err(LookupError::Escape),
            "a parent step past the root combined with a lookup is an escape"
        );
        assert_eq!(
            files.lookup(certs, "../../../.."),
            Ok(Node::ROOT),
            "climbing past the root without naming anything lands on the root, as at the root"
        );
        assert_eq!(
            files.lookup(certs, "../../../../etc/hostname"),
            Err(LookupError::Escape),
            "climbing past the root to name something is an escape from a subdirectory too"
        );
    }

    #[test]
    fn a_path_through_a_file_is_not_a_directory() {
        let files = bundle();
        assert_eq!(
            files.lookup(Node::ROOT, "etc/hostname/x"),
            Err(LookupError::NotDirectory),
            "a file has no entries beneath it"
        );
        let hostname = files.lookup(Node::ROOT, "etc/hostname").expect("hostname");
        assert_eq!(
            files.lookup(hostname, "."),
            Err(LookupError::NotDirectory),
            "a lookup relative to a file descriptor cannot proceed"
        );
    }

    #[test]
    fn undeclared_siblings_and_parents_do_not_exist() {
        let files = bundle();
        for path in [
            "etc/passwd",
            "var",
            "etc/ssl/private",
            "usr/share/zoneinfo/Etc",
        ] {
            assert_eq!(
                files.lookup(Node::ROOT, path),
                Err(LookupError::NoEntry),
                "`{path}` is not declared and must not exist"
            );
        }
    }

    #[test]
    fn record_words_round_trip_and_out_of_range_indices_are_refused() {
        let files = bundle();
        let file = files.lookup(Node::ROOT, "etc/hostname").expect("hostname");
        let directory = files.lookup(Node::ROOT, "usr/share").expect("share");
        for node in [Node::ROOT, directory, file] {
            assert_eq!(
                files.decode(node.word()),
                Some(node),
                "a record word must name the node it was written for"
            );
        }
        assert_eq!(
            files.decode(Node::directory(99).word()),
            None,
            "a directory index outside the set is not a node"
        );
        assert_eq!(
            files.decode(Node::file(3).word()),
            None,
            "a file index outside the set is not a node"
        );
        assert_eq!(
            PlatformFiles::default().decode(Node::file(0).word()),
            None,
            "the empty set has no file nodes"
        );
    }
}

mod file_bytes {
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    pub fn serialize<S: serde::Serializer>(
        bytes: &Arc<[u8]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        bytes.as_ref().serialize(serializer)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<[u8]>, D::Error> {
        Vec::<u8>::deserialize(deserializer).map(Arc::from)
    }
}
