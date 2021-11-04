//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Implements a data structure called [`PathTree`] which can
//! efficiently store many paths. It works by deduplicating common path
//! components and interning them such that a full path can be identified by a
//! single 32-bit integer.

use std::cell::RefCell;
use std::cmp::{Ord, Ordering};
use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

/// An opaque reference to a [`Path`] stored in a [`PathPool`].
#[derive(Hash, Serialize, Deserialize, Debug, PartialEq, Eq, Clone, Copy)]
pub struct PathHandle(u32);

/// PathPool exists to intern strings representing paths. It enables an
/// efficient representation of specific paths using a [`PathHandle`].
pub trait PathPool {
    /// The number of unique file paths in the tree.
    fn file_count(&self) -> usize;
    /// Stores the specified file path.
    fn create_file<P: AsRef<Path>>(&mut self, path: P);
    /// Returns true if [`PathPool::commit_changes`] needs to be called due to a previous
    /// mutating change.
    fn is_dirty(&self) -> bool;
    /// Updates the internal structures of `self` after mutating changes to the
    /// tree, ensuring that any mutating changes have been committed.
    ///
    /// [`PathPool::commit_changes`] must be run after a mutation and old [`PathHandle`]s
    /// should be discarded.
    fn commit_changes(&mut self);
    /// Returns the handle corresponding to the given path.
    fn find<S: AsRef<OsStr>, I: Iterator<Item = S>>(
        &self,
        path_components: I,
    ) -> Option<PathHandle>;
    /// Creates a handle-to-path hash map.
    fn create_lookup(&self) -> HashMap<PathHandle, OsString>;
}

/// A structure for efficient storage of file paths, implementing the PathPool
/// trait. It has an efficient representation in memory and can be serialized to
/// disk. Conceptually the data structure is a trie over path components.
#[derive(Serialize, Deserialize, Debug)]
pub struct PathTree {
    file_count: usize,
    root: TreeNodeRc,
    // Flag indicating whether the tree was mutated and [`commit_changes`] needs
    // to be run.
    #[serde(skip)]
    is_dirty: bool,
}

impl PathTree {
    /// Creates a new empty PathTree.
    pub fn new() -> Self {
        Self {
            file_count: 0,
            root: Rc::new(RefCell::new(TreeNode::Directory(DirectoryNode::new(
                "".as_ref(),
            )))),
            is_dirty: false,
        }
    }

    /// Traverses all paths in an unspecified but stable order.
    fn traverse_paths<F>(&self, mut f: F)
    where
        F: FnMut(&OsStr),
    {
        let mut s = vec![(OsString::new(), self.root.clone())];
        while !s.is_empty() {
            let (parent, node) = s.pop().unwrap();
            let node_ref = node.borrow();
            match *node_ref {
                TreeNode::Directory(ref dir) => {
                    for child in &dir.children {
                        let mut path = parent.clone();
                        if !path.is_empty() {
                            path.push(std::path::MAIN_SEPARATOR.to_string());
                        }
                        path.push(&dir.name);
                        s.push((path, child.clone()));
                    }
                }
                TreeNode::File(ref file) => {
                    let mut path = parent.clone();
                    if !path.is_empty() {
                        path.push(std::path::MAIN_SEPARATOR.to_string());
                    }
                    path.push(&file.name);
                    f(&path);
                }
            }
        }
    }

    /// Traverses all nodes in an unspecified but stable order.
    ///
    /// The relative order of traversed file nodes is guaranteed to be the same
    /// as the order of paths traversed by `traverse()`.
    ///
    /// This is because the path handles are synthesized from the items are
    /// ordered by traverse() and assigned using traverse_mut(). These handles
    /// are not written to disk and hence the order or traversal must match.
    fn traverse_mut<F>(&mut self, mut f: F)
    where
        F: FnMut(&TreeNodeRc),
    {
        let mut s = vec![self.root.clone()];
        while !s.is_empty() {
            let node = s.pop().unwrap();
            let node_ref = node.borrow();
            if let TreeNode::Directory(ref dir) = *node_ref {
                for child in &dir.children {
                    s.push(child.clone());
                }
            }
            drop(node_ref);
            f(&node);
        }
    }
}

impl Default for PathTree {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PathTree {
    fn clone(&self) -> Self {
        Self {
            file_count: self.file_count,
            root: Rc::new((*self.root).clone()),
            is_dirty: self.is_dirty,
        }
    }
}

impl PathPool for PathTree {
    fn file_count(&self) -> usize {
        self.file_count
    }

    fn is_dirty(&self) -> bool {
        self.is_dirty
    }

    fn create_file<P: AsRef<Path>>(&mut self, path: P) {
        let mut path_components = path.as_ref().components().collect::<Vec<_>>();
        assert!(
            !path_components.is_empty(),
            "Cannot add the path to the pool: the path is empty!"
        );
        let filename = path_components.pop().unwrap();

        let dir = DirectoryNode::create_dir_all(&self.root, path_components.into_iter());
        let mut dir_ref = dir.borrow_mut();
        if let TreeNode::Directory(ref mut dir) = *dir_ref {
            if dir.create_file(filename.as_ref()) {
                self.file_count += 1;
            }
        }
    }

    fn commit_changes(&mut self) {
        // To update the tree, we simply iterate over it in pre-order and assign
        // consecutive numeric values to the path handles, starting at 1. 0 is
        // the in-memory representation used for an uninitialised handle.
        let mut last_handle = PathHandle(1);
        let last_handle_ref = &mut last_handle;
        self.traverse_mut(|node| {
            let mut node_ref = node.borrow_mut();
            if let TreeNode::File(ref mut file) = *node_ref {
                file.handle = Some(*last_handle_ref);
                *last_handle_ref = PathHandle(last_handle_ref.0 + 1);
            }
        });
        self.is_dirty = false;
    }

    fn find<S: AsRef<OsStr>, I: Iterator<Item = S>>(
        &self,
        path_components: I,
    ) -> Option<PathHandle> {
        assert!(
            !self.is_dirty,
            "Tree is dirty, run commit_changes after mutating!"
        );
        let mut path_components = path_components.collect::<Vec<_>>();
        assert!(
            !path_components.is_empty(),
            "Cannot add the path to the pool: the path is empty!"
        );
        let filename = path_components.pop().unwrap();

        let dir = DirectoryNode::open_dir_all(&self.root, path_components.into_iter());
        dir.as_ref()?;
        let dir = dir.unwrap();

        let dir_ref = dir.borrow();
        if let TreeNode::Directory(ref dir) = *dir_ref {
            let node = dir.open(filename.as_ref());
            // Just take the handle from the file node.
            return node
                .map(|x| match &*x.borrow() {
                    TreeNode::File(file) => file.handle,
                    _ => unreachable!(),
                })
                .flatten();
        }

        None
    }

    fn create_lookup(&self) -> HashMap<PathHandle, OsString> {
        assert!(
            !self.is_dirty,
            "Tree is dirty, run commit_changes after mutating!"
        );
        let mut map = HashMap::new();

        let mut i = 1;
        self.traverse_paths(|x| {
            map.insert(PathHandle(i), x.into());
            i += 1;
        });

        map
    }
}

/// A node in the path tree.
///
/// Note: Path names are stored as UTF-8 encoded strings.
#[derive(Clone, Serialize, Deserialize, Debug)]
enum TreeNode {
    Directory(DirectoryNode),
    File(FileNode),
}

type TreeNodeRc = Rc<RefCell<TreeNode>>;

#[derive(Serialize, Deserialize, Debug)]
struct DirectoryNode {
    name: OsString,
    children: BTreeSet<TreeNodeRc>,
}

impl Clone for DirectoryNode {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            children: self
                .children
                .iter()
                .map(|x| Rc::new((**x).clone()))
                .collect(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct FileNode {
    name: OsString,
    #[serde(skip)]
    handle: Option<PathHandle>,
}

impl TreeNode {
    fn name(&self) -> &OsStr {
        match self {
            TreeNode::Directory(ref d) => d.name(),
            TreeNode::File(ref f) => f.name(),
        }
    }
}

impl Ord for TreeNode {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name().cmp(other.name())
    }
}

impl PartialOrd for TreeNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TreeNode {
    fn eq(&self, other: &Self) -> bool {
        self.name() == other.name()
    }
}

impl Eq for TreeNode {}

impl DirectoryNode {
    fn new(name: &OsStr) -> Self {
        Self {
            name: name.into(),
            children: BTreeSet::new(),
        }
    }

    fn name(&self) -> &OsStr {
        &self.name
    }

    /// Opens a node in the current directory.
    fn open(&self, name: &OsStr) -> Option<TreeNodeRc> {
        self.children
            .iter()
            .find(|x| x.borrow().name() == name)
            .cloned()
    }

    /// Creates a subdirectory.
    fn create_dir(&mut self, name: &OsStr) -> TreeNodeRc {
        let existing = self.open(name);
        if let Some(e) = existing {
            return e;
        }

        let node = Rc::new(RefCell::new(TreeNode::Directory(DirectoryNode::new(name))));
        assert!(self.children.insert(node.clone()));
        node
    }

    /// Opens a directory that is several levels deep.
    fn open_dir_all<S: AsRef<OsStr>, I: Iterator<Item = S>>(
        root: &TreeNodeRc,
        mut components: I,
    ) -> Option<TreeNodeRc> {
        let root_ref = root.borrow();
        if let TreeNode::Directory(ref dir) = *root_ref {
            if let Some(c) = components.next() {
                if let Some(node) = dir.open(c.as_ref()) {
                    return DirectoryNode::open_dir_all(&node, components);
                } else {
                    return None;
                }
            }
            Some(root.clone())
        } else {
            panic!("Root is not a directory!")
        }
    }

    /// Recursively creates all directories.
    fn create_dir_all<S: AsRef<OsStr>, I: Iterator<Item = S>>(
        root: &TreeNodeRc,
        mut components: I,
    ) -> TreeNodeRc {
        let mut root_ref = root.borrow_mut();
        if let TreeNode::Directory(ref mut dir) = *root_ref {
            if let Some(c) = components.next() {
                if let Some(node) = dir.open(c.as_ref()) {
                    return DirectoryNode::create_dir_all(&node, components);
                } else {
                    let subdir = dir.create_dir(c.as_ref());
                    return DirectoryNode::create_dir_all(&subdir, components);
                }
            }
            root.clone()
        } else {
            panic!("Root is not a directory!")
        }
    }

    /// Creates a file entry. Returns true if a new entry was added,
    /// false if the entry was already present.
    fn create_file(&mut self, name: &OsStr) -> bool {
        let existing = self.open(name);
        if existing.is_some() {
            return false;
        }

        let node = Rc::new(RefCell::new(TreeNode::File(FileNode::new(name))));
        assert!(self.children.insert(node));
        true
    }
}

impl FileNode {
    fn new(name: &OsStr) -> Self {
        Self {
            name: name.into(),
            handle: None,
        }
    }

    fn name(&self) -> &OsStr {
        &self.name
    }
}
