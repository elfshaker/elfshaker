//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::{
    cell::RefCell,
    collections::hash_map::Entry,
    collections::HashMap,
    error::Error,
    fs::{create_dir_all, read_link, write, File, Metadata},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use clap::{App, Arg};
use crypto::digest::Digest;
use crypto::sha1::Sha1;
use rayon::prelude::*;
use thread_local::ThreadLocal;
use walkdir::WalkDir;
use zstd::stream::raw::{CParameter, DParameter};
use zstd::{Decoder, Encoder};

use elfshaker::packidx::{
    ObjectChecksum, ObjectIndex, PackError, PackIndex, PackedFile, PackedFileList, Snapshot,
};
use elfshaker::pathidx::{PathError, PathIndex, PathTree};

#[derive(Clone, Debug)]
struct InputFileEntry {
    paths: Vec<PathBuf>,
    content_path: PathBuf,
    checksum: [u8; 20],
    offset: u64,
    size: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let matches = App::new("create-pack")
        .arg(Arg::with_name("INPUT").required(true).index(1))
        .arg(Arg::with_name("OUTPUT").required(true).index(2))
        .arg(Arg::with_name("strip-components")
            .required(true)
            .takes_value(true)
            .long("strip-components")
            .help("Specifies now many levels deep relative to the input directory the sets of files to be included in each snapshot are found."))
        .arg(Arg::with_name("compression-level")
            .required(true)
            .takes_value(true)
            .long("compression-level"))
        .arg(
            Arg::with_name("validate")
                .takes_value(true)
                .long("validate") // --validate=04T182935-99f74a64a2dd
                .help("The snapshot tag to extract after packing (for validation purposes)"),
        )
        .get_matches();

    let input_dir = Path::new(matches.value_of("INPUT").unwrap());
    let output = matches.value_of("OUTPUT").unwrap();
    let validate_tag = matches.value_of("validate");
    let path_depth: u32 = matches.value_of("strip-components").unwrap().parse()?;
    let compression_level: i32 = matches.value_of("compression-level").unwrap().parse()?;
    let pack_index_path = String::from(output) + ".idx";

    if path_depth == 0 {
        panic!("--strip-components value must be > 1!");
    }

    if let Some(validate_tag) = validate_tag {
        println!(
            "Snapshot with tag {:?} will be decompressed after packing for validation purposes.",
            validate_tag
        );
    }

    println!("Enumerating directory entries and collecting metadata...");

    let dir_iter = walk_follow_symlinks(input_dir)?;

    println!("Creating auxiliary structures");

    let entries_iter = dir_iter.map(|x| -> io::Result<InputFileEntry> {
        Ok(InputFileEntry {
            paths: x.symlinks,
            content_path: x.original,
            size: x.metadata.len(),
            // will be computed later
            checksum: [0; 20],
            offset: 0,
        })
    });

    let mut objects: Vec<_> = entries_iter.collect::<io::Result<Vec<_>>>()?;

    println!("Sorting the entries...");
    // Sorting by filename
    // objects.sort_by(|x, y| {
    //     Iterator::cmp(
    //         x.paths.first().unwrap().components(),
    //         y.paths.first().unwrap().components(),
    //     )
    // });
    // Sorting by filesize -- seems to work better
    objects.sort_by_key(|x| x.size);

    // Fix offsets
    let mut offset = 0;
    for entry in &mut objects {
        entry.offset = offset;
        offset += entry.size;
    }

    println!("Computing checksums...");
    let object_paths = objects.iter().map(|x| &x.content_path).collect::<Vec<_>>();
    let file_paths = objects
        .iter()
        .map(|x| &x.paths)
        .flatten()
        .collect::<Vec<_>>();
    // Create path tree
    println!("Constructing tree...");
    let tree = create_path_tree(&Path::new(input_dir), path_depth, &file_paths).expect("Bad path!");
    // Compute and assign checksums
    let checksums = compute_checksums(&object_paths)?;
    for (checksum, entry) in checksums.iter().zip(objects.iter_mut()) {
        entry.checksum = *checksum;
    }

    println!("Constructing snapshots...");
    let mut snapshots = create_snapshots_from_paths(input_dir, path_depth, &tree, &objects)?;
    // Sort snapshots by tag
    snapshots.sort_by(|x, y| x.tag.cmp(&y.tag));

    println!("Creating the following {} snapshots: ", snapshots.len());
    for c in &snapshots {
        println!("{}", c.tag);
    }

    println!("Computing file changes between snapshots...");
    let snapshots =
        Snapshot::compute_deltas(snapshots.iter()).expect("Failed to compute file changes!");

    println!("Writing index...");
    let mut pack_index_file = File::create(String::from(output) + ".idx")?;

    let object_index = objects
        .iter()
        .map(|x| ObjectIndex {
            checksum: x.checksum,
            offset: x.offset,
            size: x.size,
        })
        .collect();

    let pack_index = PackIndex::new(tree, object_index, snapshots);
    rmp_serde::encode::write(&mut pack_index_file, &pack_index)?;

    let pack_file = File::create(output)?;
    let object_paths = objects.iter().map(|x| &x.content_path).collect::<Vec<_>>();
    create_pack(pack_file, compression_level, &object_paths)?;

    // Validation
    if let Some(validate_tag) = validate_tag {
        let output_dir = PathBuf::from(String::from(output) + ".dir/");
        create_dir_all(&output_dir)?;

        let start_time = Instant::now();
        println!("Decompressing back into {:?}", &output_dir);
        unpack_snapshot(
            File::open(output)?,
            File::open(pack_index_path)?,
            validate_tag,
            output_dir,
        )?;
        let time_to_decompress = (Instant::now() - start_time).as_secs_f32();
        // This number give us an upper bound on how quickly (after reading the index)
        // we can materialise a single build.
        println!("Decompression took {}s", time_to_decompress);
    }

    Ok(())
}

struct FileEntry {
    original: PathBuf,
    metadata: Metadata,
    symlinks: Vec<PathBuf>,
}

/// Unpacks the snapshot with the given tag to the output directory.
fn unpack_snapshot<R1: Read, R2: Read, P: AsRef<Path>>(
    pack: R1,
    packidx: R2,
    snapshot_tag: &str,
    output_dir: P,
) -> Result<(), Box<dyn Error>> {
    let packidx: PackIndex = rmp_serde::decode::from_read(packidx)?;
    let file_list;
    if let Some(snapshot) = packidx.find_snapshot(snapshot_tag) {
        // PackIndex::find_snapshot returns snapshots with complete lists only
        file_list = match snapshot.list {
            PackedFileList::Complete(c) => c,
            _ => unreachable!(),
        }
    } else {
        panic!("Snapshot not found!");
    }

    let path_lookup = packidx.tree().create_lookup();
    // Map the indexes to the correspondng path and object info.
    let mut entries = file_list
        .iter()
        .map(|x| {
            (
                path_lookup[&x.tree_index()].clone(),
                packidx.objects()[x.object_index() as usize].clone(),
            )
        })
        .collect::<Vec<_>>();

    // Sort objects to allow for one-way seeking
    entries.sort_by_key(|(_, o)| o.offset);
    println!("Decompressing {} files...", entries.len());

    let mut decoder = Decoder::new(pack)?;
    // 2^30 = 1024MiB window log
    decoder.set_parameter(DParameter::WindowLogMax(30))?;
    let mut buf = vec![];
    let mut path_buf = PathBuf::new();
    let mut pos = 0;

    for (path, object) in entries {
        let discard_bytes = object.offset - pos;
        io::copy(&mut decoder.by_ref().take(discard_bytes), &mut io::sink())?;
        // Resize buf
        if buf.len() < object.size as usize {
            buf.resize(object.size as usize, 0);
        }
        // Read object
        decoder.read_exact(&mut buf[..object.size as usize])?;
        pos = object.offset + object.size;
        // Verify checksum
        let mut checksum = [0u8; 20];
        let mut hasher = Sha1::new();
        hasher.input(&buf[..object.size as usize]);
        hasher.result(&mut checksum);
        if object.checksum != checksum {
            return Err(Box::new(PackError::ChecksumMismatch));
        }
        // Output path
        path_buf.clear();
        path_buf.push(&output_dir);
        path_buf.push(snapshot_tag);
        path_buf.push(path);
        create_dir_all(path_buf.parent().unwrap())?;
        write(&path_buf, &buf)?;
    }

    Ok(())
}

/// Creates a PathTree from the given list of paths and using the specified options.
fn create_path_tree<P>(
    root_path: &Path,
    path_depth: u32,
    paths: &[P],
) -> Result<PathTree, PathError>
where
    P: AsRef<Path>,
{
    let mut tree = PathTree::new();

    // Create a path in the tree for each path referencing this object
    for path in paths {
        let components: Result<Vec<_>, PathError> = path
            .as_ref()
            .strip_prefix(root_path)
            .map_err(|_| PathError::InvalidPath)?
            .components()
            .skip(path_depth as usize)
            .map(|x| x.as_os_str().to_str().ok_or(PathError::InvalidPath))
            .collect();
        tree.create_file(components?.iter())?;
    }

    println!("Tree contains {} file objects.", tree.file_count());

    tree.update_index();

    Ok(tree)
}

/// Creates a list of snapshots from the given files and options.
fn create_snapshots_from_paths(
    input_dir: &Path,
    path_depth: u32,
    tree: &PathTree,
    objects: &[InputFileEntry],
) -> Result<Vec<Snapshot>, PackError> {
    let mut snapshots: HashMap<String, Vec<PackedFile>> = HashMap::new();
    // Use the --path-depth value to create tags/names for the snapshots.
    for (object_index, object) in objects.iter().enumerate() {
        for path in &object.paths {
            let rel_path = path
                .strip_prefix(input_dir)
                .map_err(|_| PackError::PathNotFound)?;
            // Use the first #path_depth directory names as the tag.
            let tag_path = rel_path
                .components()
                .take(path_depth as usize)
                .map(|x| x.as_os_str().to_str().unwrap())
                .collect();
            let in_tree_path = rel_path
                .components()
                .skip(path_depth as usize)
                .map(|x| x.as_os_str());

            let tree_index = tree
                .find_index(in_tree_path)
                .map_err(|_| PackError::PathNotFound)?
                .ok_or(PackError::PathNotFound)?;
            let packed_file = PackedFile::new(tree_index, object_index as u32);

            match snapshots.entry(tag_path) {
                Entry::Occupied(mut o) => {
                    o.get_mut().push(packed_file);
                }
                Entry::Vacant(v) => {
                    v.insert(vec![packed_file]);
                }
            }
        }
    }
    Ok(snapshots
        .into_iter()
        .map(|(key, value)| Snapshot {
            tag: key,
            list: PackedFileList::Complete(value),
        })
        .collect())
}

/// Concats the contents of the files at the provided paths and Zstandard compresses it.
fn create_pack<W, P>(pack_file: W, compression_level: i32, objects_paths: &[P]) -> io::Result<()>
where
    W: Write,
    P: AsRef<Path>,
{
    let mut encoder = Encoder::new(pack_file, compression_level)?;
    // NbWorkers requires the zstdmt features to be enabled.
    encoder.set_parameter(CParameter::NbWorkers(num_cpus::get_physical() as u32))?;
    encoder.set_parameter(CParameter::EnableLongDistanceMatching(true))?;
    // 2^30 = 1024MiB window log
    encoder.set_parameter(CParameter::WindowLog(30))?;

    println!("Writing pack file...");

    let mut processed_bytes = 0;

    for (i, obj) in objects_paths.iter().enumerate() {
        let mut file = File::open(&obj)?;
        let bytes = io::copy(&mut file, &mut encoder)?;
        processed_bytes += bytes;
        if i % 100 == 0 {
            println!(
                "File no. {} of {} ({} bytes)",
                i,
                objects_paths.len(),
                bytes
            );
        }
    }

    encoder.finish()?;
    println!("Processed {} bytes!", processed_bytes);

    Ok(())
}

/// Computes the content checksums of the files at the listed paths.
fn compute_checksums<P>(paths: &[P]) -> io::Result<Vec<ObjectChecksum>>
where
    P: AsRef<Path> + Sync,
{
    let tls_buf = ThreadLocal::new();
    paths
        .par_iter()
        .map(|x| {
            let mut buf = tls_buf.get_or(|| RefCell::new(vec![])).borrow_mut();
            buf.clear();

            let mut file = File::open(&x)?;
            file.read_to_end(&mut buf)?;

            let checksum_buf = &mut [0u8; 20];
            let mut hasher = Sha1::new();
            hasher.input(&buf);
            hasher.result(checksum_buf);
            Ok(*checksum_buf)
        })
        .collect::<io::Result<Vec<_>>>()
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
