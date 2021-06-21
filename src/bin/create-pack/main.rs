//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::{create_dir_all, read_link, write, File, Metadata},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    rc::Rc,
    time::{Instant, SystemTime},
};

use clap::{App, Arg};
use crypto::digest::Digest;
use crypto::sha1::Sha1;
use rayon::prelude::*;
use thread_local::ThreadLocal;
use walkdir::WalkDir;
use zstd::stream::raw::{CParameter, DParameter};
use zstd::{Decoder, Encoder};

#[derive(Clone, Debug)]
struct Object {
    paths: Vec<PathBuf>,
    content_path: PathBuf,
    modified: SystemTime,
    checksum: [u8; 20],
    offset: u64,
    size: u64,
}

#[derive(Clone, Debug)]
struct FileNode {
    name: OsString,
    index: u32,
}

#[derive(Clone, Debug)]
struct DirNode {
    name: OsString,
    children: Vec<Rc<RefCell<Tree>>>,
}

#[derive(Clone, Debug)]
enum Tree {
    Dir(DirNode),
    File(FileNode),
}

impl Tree {
    fn is_dir(&self) -> bool {
        matches!(self, Tree::Dir(_))
    }

    fn is_file(&self) -> bool {
        matches!(self, Tree::File(_))
    }

    fn unwrap_dir(&self) -> &DirNode {
        match self {
            Tree::Dir(ref dir) => dir,
            _ => panic!("Not a dir node!"),
        }
    }

    fn unwrap_file(&self) -> &FileNode {
        match self {
            Tree::File(ref file) => file,
            _ => panic!("Not a file node!"),
        }
    }
}

fn main() -> io::Result<()> {
    let matches = App::new("create-pack")
        .arg(Arg::with_name("INPUT").required(true).index(1))
        .arg(Arg::with_name("OUTPUT").required(true).index(2))
        .arg(
            Arg::with_name("validate")
                .takes_value(true)
                .long("validate") // --validate=01/T034951-4d7201e7b988
                .help("The directory to extract after packing (for validation purposes)"),
        )
        .get_matches();

    let input_dir = matches.value_of("INPUT").unwrap();
    let output = matches.value_of("OUTPUT").unwrap();
    let validate_path = matches.value_of("validate").map(PathBuf::from);

    if let Some(validate_path) = validate_path.as_ref() {
        println!(
            "Entries under {:?} will be decompressed after packing for validation purposes.",
            validate_path
        );
    }

    println!("Enumerating directory entries and collecting metadata...");

    let dir_iter = walk_follow_symlinks(input_dir)?;

    println!("Creating auxiliary structures");

    let entries_iter = dir_iter
        .map(|x| -> io::Result<Object> {
            Ok(Object {
                paths: x.symlinks,
                content_path: x.original,
                modified: x.metadata.modified()?,
                checksum: [0; 20],
                offset: 0,
                size: x.metadata.len(),
            })
        });

    let mut objects: Vec<_> = entries_iter.collect::<io::Result<Vec<_>>>()?;

    println!("Sorting the entries...");
    // Sorting by filename
    objects.sort_by(|x, y| {
        Iterator::cmp(
            x.paths.first().unwrap().components(),
            y.paths.first().unwrap().components(),
        )
    });

    // Fix offsets
    let mut offset = 0;
    for entry in &mut objects {
        entry.offset = offset;
        offset += entry.size;
    }

    println!("Computing checksums...");
    let tls_buf = ThreadLocal::new();
    let checksums: Vec<[u8; 20]> = objects
        .par_iter()
        .map(|x| {
            let mut buf = tls_buf.get_or(|| RefCell::new(vec![])).borrow_mut();
            buf.clear();

            let mut file = File::open(&x.content_path)?;
            file.read_to_end(&mut buf)?;

            let checksum_buf = &mut [0u8; 20];
            let mut hasher = Sha1::new();
            hasher.input(&buf);
            hasher.result(checksum_buf);
            Ok(*checksum_buf)
        })
        .collect::<io::Result<Vec<_>>>()?;

    for (checksum, entry) in checksums.iter().zip(objects.iter_mut()) {
        entry.checksum = *checksum;
    }

    println!("Constructing tree...");
    let root = create_file_tree(&Path::new(input_dir), &mut objects);

    println!("Writing index...");
    let pack_index_file = File::create(String::from(output) + ".idx")?;
    write_metadata(pack_index_file, &objects, &root)?;

    let pack_file = File::create(output)?;
    let mut encoder = Encoder::new(pack_file, 10)?;
    encoder.set_parameter(CParameter::EnableLongDistanceMatching(true))?;
    // 2^28 = 256GMiB window log
    encoder.set_parameter(CParameter::WindowLog(28))?;
    // 8 worker threads -- fails at runtime: not supported ???
    // encoder.set_parameter(CParameter::NbWorkers(8))?;

    println!("Writing pack file...");

    let mut processed = 0;
    let mut processed_bytes = 0;

    for obj in &objects {
        let mut file = File::open(&obj.content_path)?;
        let bytes = io::copy(&mut file, &mut encoder)?;
        processed += 1;
        processed_bytes += bytes;
        println!("{} of {} ({} bytes)", processed, objects.len(), bytes);
    }

    encoder.finish()?;
    
    println!("Processed {} bytes!", processed_bytes);

    // Validation
    if let Some(validate_path) = validate_path {
        let output_dir = PathBuf::from(String::from(output) + ".dir/");
        create_dir_all(&output_dir)?;

        let start_time = Instant::now();
        println!("Decompressing back into {:?}", &output_dir);
        unpack_dir(
            Path::new(input_dir),
            File::open(output)?,
            &validate_path,
            &objects,
            &output_dir,
        )?;
        let time_to_decompress = (Instant::now() - start_time).as_secs_f32();
        // This number give us an upper bound on how quickly (after reading the index)
        // we can materialise a single build.
        // This is actually very suboptimal ATM, but shows 10s for a single build on my machine.
        // The suboptimal part has to do with Rust iterators and decompressing all blocks individually.
        println!("Decompression took {}s", time_to_decompress);
    }

    Ok(())
}

struct FileEntry {
    original: PathBuf,
    metadata: Metadata,
    symlinks: Vec<PathBuf>,
}

fn unpack_dir<R: Read, P: AsRef<Path>>(
    source_dir: P,
    packfile: R,
    match_path: P,
    objects: &[Object],
    output_dir: P,
) -> io::Result<()> {
    let mut decoder = Decoder::new(packfile)?;
    // 2^28 = 256GMiB window log
    decoder.set_parameter(DParameter::WindowLogMax(28))?;
    // 8 worker threads -- fails at runtime: not supported ???
    // decoder.set_parameter(DParameter::NbWorkers(8))?;
    let mut buf = vec![];

    for obj in objects {
        if buf.len() < obj.size as usize {
            buf.resize(obj.size as usize, 0);
        }
        decoder.read_exact(&mut buf[..obj.size as usize])?;

        for path in &obj.paths {
            let rel_path = path
                .components()
                .skip(source_dir.as_ref().components().count())
                .collect::<PathBuf>();

            let match_c = match_path.as_ref().components();
            if Iterator::eq(rel_path.components().take(match_c.clone().count()), match_c) {
                let mut file_path: PathBuf = output_dir.as_ref().into();
                file_path.push(rel_path.to_str().unwrap());
                create_dir_all(file_path.parent().unwrap())?;
                write(&file_path, &buf[..obj.size as usize])?;
            }
        }
    }

    Ok(())
}

fn create_file_tree(
    root_path: &Path,
    objects: &mut Vec<Object>,
) -> std::rc::Rc<std::cell::RefCell<Tree>> {
    let root = Rc::new(RefCell::new(Tree::Dir(DirNode {
        name: Default::default(),
        children: vec![],
    })));

    // Indexes start from 1;
    // 0 is reserved as a sentinel value when serialising a sequence of these indexes. 
    let mut path_index = 1;
    for obj in objects.iter() {
        // Create a path in the tree for each path referencing this object
        for path in &obj.paths {
            let components = path.components();
            let mut components: Vec<_> = remove_prefix(&components, root_path.components())
                .map(|c| c.as_os_str())
                .collect();

            let filename = components.pop().unwrap().to_os_string();

            let dir = create_dir_all_node(&root, components.into_iter());
            create_file_node(
                &dir,
                FileNode {
                    name: filename.to_os_string(),
                    index: path_index, // assign unique index to each leaf node
                },
            );

            path_index += 1;
        }
    }

    println!("Tree contains {} file objects.", path_index - 1);

    root
}

fn remove_prefix<P: AsRef<Path>>(path: &P, prefix: P) -> impl Iterator<Item = Component> {
    let path = path.as_ref();
    let prefix = prefix.as_ref();

    assert!(Iterator::eq(
        path.components().take(prefix.components().count()),
        prefix.components()
    ));

    path.components().skip(prefix.components().count())
}

fn walk_follow_symlinks<P: AsRef<Path>>(
    input_dir: P,
) -> io::Result<impl Iterator<Item = FileEntry>> {
    let mut file_list = WalkDir::new(input_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() || e.file_type().is_symlink())
        .collect::<Vec<_>>();

    println!("Sorting entries...");
    file_list.sort_by(|x, y| x.path().cmp(y.path()));

    println!("Resolving symlinks...");
    let mut entry_map: HashMap<PathBuf, FileEntry> = HashMap::new();

    for (i, e) in file_list.iter().enumerate() {
        if e.file_type().is_symlink() {
            let target_path = read_link(e.path())?;
            let real_path = PathBuf::from(e.path()).parent().unwrap().join(&target_path);
            if i % 10000 == 0 {
                // Track progress
                println!("{:?} ==> {:?}", e.path(), &real_path);
            }
            if let Some(ref mut existing) = entry_map.get_mut(&target_path) {
                existing.symlinks.push(e.path().into());
            } else {
                let metadata = std::fs::metadata(&real_path)?;
                entry_map.insert(
                    target_path.clone(),
                    FileEntry {
                        original: real_path,
                        metadata,
                        symlinks: vec![e.path().into()],
                    },
                );
            }
        } else if e.file_type().is_file() {
            let metadata = std::fs::metadata(e.path())?;
            entry_map.insert(
                e.path().into(),
                FileEntry {
                    original: e.path().into(),
                    metadata,
                    symlinks: vec![],
                },
            );
        } else {
            unreachable!("Entry type unexpected at this stage!");
        }
    }

    Ok(entry_map.into_iter().map(|x| x.1))
}

fn create_file_node(dir: &Rc<RefCell<Tree>>, file: FileNode) -> Rc<RefCell<Tree>> {
    if let Tree::Dir(ref mut dir) = *dir.borrow_mut() {
        let node = Rc::new(RefCell::new(Tree::File(file)));
        dir.children.push(node.clone());
        node
    } else {
        panic!("Root is not a directory!")
    }
}

fn create_dir_all_node<'a, I: Iterator<Item = &'a OsStr>>(
    root: &Rc<RefCell<Tree>>,
    mut components: I,
) -> Rc<RefCell<Tree>> {
    let root_ref = root.borrow();
    if let Tree::Dir(ref dir) = *root_ref {
        if let Some(c) = components.next() {
            if let Some(node) = find_dir_node(dir, c) {
                return create_dir_all_node(&node, components);
            } else {
                drop(root_ref);
                let subdir = create_dir_node(root, c);
                return create_dir_all_node(&subdir, components);
            }
        }
        root.clone()
    } else {
        panic!("Root is not a directory!")
    }
}

fn find_dir_node(root: &DirNode, name: &OsStr) -> Option<Rc<RefCell<Tree>>> {
    let n = root.children.iter().find(|x| match &*x.borrow() {
        Tree::Dir(DirNode {
            name: _name,
            ..
        }) => _name == name,
        _ => false,
    });

    n?;

    if let Tree::Dir(_) = *n.unwrap().borrow_mut() {
        return Some(n.unwrap().clone());
    }
    None
}

fn create_dir_node(root: &Rc<RefCell<Tree>>, name: &OsStr) -> Rc<RefCell<Tree>> {
    if let Tree::Dir(ref mut dir) = *root.borrow_mut() {
        dir.children.push(Rc::new(RefCell::new(Tree::Dir(DirNode {
            name: name.to_os_string(),
            children: vec![],
        }))));
        dir.children.last().unwrap().clone()
    } else {
        panic!("Node is not a directory!")
    }
}

const FILE_NODE: &[u8] = &[b'F'];
const DIR_NODE: &[u8] = &[b'D'];
const EXIT_NODE: &[u8] = &[b';'];

fn write_metadata(
    mut writer: impl Write,
    objects: &[Object],
    tree: &Rc<RefCell<Tree>>,
) -> io::Result<()> {
    // Serialise and write the tree
    let mut tree_buf = vec![];
    println!("Serialising tree...");
    write_tree(&mut tree_buf, tree)?;
    let tree_size = tree_buf.len() as u32;
    println!("Tree metadata is {} bytes", tree_size);
    writer.write_all(&tree_size.to_be_bytes())?;
    writer.write_all(&tree_buf)?;

    // Write the sorted entries
    let num_entries = (objects.len() * 20) as u32;
    writer.write_all(&num_entries.to_be_bytes())?;
    for obj in objects {
        writer.write_all(&obj.checksum)?;
        writer.write_all(&obj.offset.to_be_bytes())?;
        writer.write_all(&obj.size.to_be_bytes())?;
        writer.write_all(&0u32.to_be_bytes())?
    }

    writer.flush()?;
    Ok(())
}

fn write_tree(writer: &mut impl Write, root: &Rc<RefCell<Tree>>) -> io::Result<()> {
    write_dir(writer, root.borrow().unwrap_dir())?;
    Ok(())
}

fn write_dir(writer: &mut impl Write, dir: &DirNode) -> io::Result<()> {
    let subdirs = dir.children.iter().filter(|x| x.borrow().is_dir());
    let files = dir.children.iter().filter(|x| x.borrow().is_file());

    writer.write_all(DIR_NODE)?;
    let dir_name = dir
        .name
        .to_str()
        .expect("Filename cannot be losslessly converted to UTF-8!");
    // Write nul-terminated UTF-8 path name.
    writer.write_all(dir_name.as_bytes())?;
    writer.write_all(&[0])?;

    for file in files {
        write_file(writer, file.borrow().unwrap_file())?;
    }

    for subdir in subdirs {
        write_dir(writer, subdir.borrow().unwrap_dir())?;
    }
    writer.write_all(EXIT_NODE)?;

    Ok(())
}

fn write_file(writer: &mut impl Write, file: &FileNode) -> io::Result<()> {
    writer.write_all(FILE_NODE)?;
    let file_name = file
        .name
        .to_str()
        .expect("Filename cannot be losslessly converted to UTF-8!");
    // Write null-terminated UTF-8 path name.
    writer.write_all(file_name.as_bytes())?;
    writer.write_all(&[0])?;
    // Write index as a 32-bit big endian number.
    writer.write_all(&file.index.to_be_bytes())?;
    Ok(())
}
