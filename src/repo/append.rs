//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

//! Same-path frame affinity, selective recompression, and size-sorted splitting.

use super::{
    algo::run_in_parallel,
    error::Error,
    pack::{write_skippable_frame, PackFrame, PackHeader},
    transaction, PackId, PackOptions, Repository,
};
use crate::{
    batch::{compress_files, CompressionOptions},
    packidx::{ObjectChecksum, PackError, PackIndex},
    progress::ProgressReporter,
};
use sha1::{Digest, Sha1};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    ops::ControlFlow,
    path::Path,
};

type Object = (ObjectChecksum, u64);

struct Frame {
    objects: Vec<Object>,
    header: PackFrame,
    // Compressed offset in the input pack; newly encoded frames have a buffer.
    offset: u64,
    buffer: Option<Vec<u8>>,
}

impl PackOptions {
    pub(super) fn fill_size(&self) -> io::Result<u64> {
        if self.num_workers == 0 || self.frame_size == 0 || !(1..=100).contains(&self.frame_fill) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "packing requires positive workers/frame size and a fill percentage in 1..=100",
            ));
        }
        // Divide first so even a caller-supplied u64::MAX target cannot overflow.
        Ok((self.frame_size / 100 * u64::from(self.frame_fill)
            + self.frame_size % 100 * u64::from(self.frame_fill) / 100)
            .max(1))
    }
}

/// Greedy partitions of an already sorted sequence. An object is indivisible,
/// so an object larger than the budget occupies a frame by itself.
pub(super) fn partition_by_budget<T>(
    objects: &[T],
    budget: u64,
    size: impl Fn(&T) -> u64,
) -> Vec<&[T]> {
    let mut groups = Vec::new();
    let mut start = 0;
    let mut bytes = 0u64;
    for (i, object) in objects.iter().enumerate() {
        let length = size(object);
        if i > start && length > budget.saturating_sub(bytes) {
            groups.push(&objects[start..i]);
            start = i;
            bytes = 0;
        }
        bytes = bytes.saturating_add(length);
    }
    if start < objects.len() {
        groups.push(&objects[start..]);
    }
    groups
}

fn read_frames(file: &mut File, index: &PackIndex) -> Result<Vec<Frame>, Error> {
    let (header, mut offset) = PackHeader::read_for_append(file)?;
    let mut objects = index
        .object_checksums()
        .map(|checksum| (*checksum, *index.object_metadata(checksum)))
        .collect::<Vec<_>>();
    objects.sort_by_key(|(checksum, md)| (md.offset, md.size, *checksum));
    let mut objects = objects.into_iter().peekable();
    let mut raw_offset = 0u64;
    let mut result = Vec::new();
    for header in header.frames {
        if header.frame_size == 0 {
            return Err(Error::CorruptPack);
        }
        let end = raw_offset
            .checked_add(header.decompressed_size)
            .ok_or(Error::CorruptPack)?;
        let mut frame_objects = Vec::new();
        while let Some((_, md)) = objects.peek() {
            if md.offset > end || (md.offset == end && md.size != 0) {
                break;
            }
            let (checksum, md) = objects.next().unwrap();
            if md.offset != raw_offset {
                return Err(Error::CorruptPackIndex);
            }
            raw_offset = raw_offset
                .checked_add(md.size)
                .filter(|n| *n <= end)
                .ok_or(Error::CorruptPackIndex)?;
            frame_objects.push((checksum, md.size));
        }
        // Reject gaps, overlaps and objects crossing a frame boundary before writing.
        if raw_offset != end {
            return Err(Error::CorruptPackIndex);
        }
        let next = offset
            .checked_add(header.frame_size)
            .ok_or(Error::CorruptPack)?;
        result.push(Frame {
            objects: frame_objects,
            header,
            offset,
            buffer: None,
        });
        offset = next;
    }
    if objects.next().is_some() {
        return Err(Error::CorruptPackIndex);
    }
    Ok(result)
}

fn nearest_frame(frames: &[Frame], size: u64) -> usize {
    frames
        .iter()
        .enumerate()
        .min_by_key(|(_, f)| {
            let lo = f.objects.iter().map(|o| o.1).min().unwrap_or(0);
            let hi = f.objects.iter().map(|o| o.1).max().unwrap_or(0);
            lo.saturating_sub(size).max(size.saturating_sub(hi))
        })
        .unwrap()
        .0
}

fn rewrite(
    frame: &Frame,
    additions: &[Object],
    previous: &Path,
    repo: &Repository,
    opts: &PackOptions,
) -> Result<Vec<Frame>, Error> {
    let mut old = Vec::new();
    if frame.header.frame_size != 0 {
        let mut file = File::open(previous)?;
        file.seek(SeekFrom::Start(frame.offset))?;
        let mut decoder = zstd::Decoder::new(file.take(frame.header.frame_size))?;
        decoder.window_log_max(30)?;
        decoder
            .take(
                frame
                    .header
                    .decompressed_size
                    .checked_add(1)
                    .ok_or(Error::CorruptPack)?,
            )
            .read_to_end(&mut old)?;
        if old.len() as u64 != frame.header.decompressed_size {
            return Err(Error::CorruptPack);
        }
    }
    let mut buffers = HashMap::new();
    let mut offset = 0usize;
    for (checksum, size) in &frame.objects {
        let end = offset + *size as usize; // read_frames and the decoded length bound this slice.
        let bytes = &old[offset..end];
        if (*checksum == [0; 20] && *size != 0)
            || (*checksum != [0; 20] && ObjectChecksum::from(Sha1::digest(bytes)) != *checksum)
        {
            return Err(Error::CorruptPack);
        }
        buffers.insert(*checksum, bytes);
        offset = end;
    }
    let mut new_buffers = Vec::new();
    for (checksum, size) in additions {
        let bytes = if *checksum == [0; 20] {
            Vec::new()
        } else {
            fs::read(repo.loose_object_path(checksum))?
        };
        if bytes.len() as u64 != *size
            || (*checksum != [0; 20] && ObjectChecksum::from(Sha1::digest(&bytes)) != *checksum)
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "corrupt loose object").into());
        }
        new_buffers.push((*checksum, bytes));
    }
    for (checksum, bytes) in &new_buffers {
        buffers.insert(*checksum, bytes);
    }
    let mut objects = frame.objects.clone();
    objects.extend_from_slice(additions);
    objects.sort_by_key(|o| (o.1, o.0));
    let size = objects.iter().try_fold(0u64, |sum, o| {
        sum.checked_add(o.1).ok_or(Error::CorruptPackIndex)
    })?;
    let budget = if size > opts.frame_size {
        opts.fill_size()?
    } else {
        u64::MAX
    };
    partition_by_budget(&objects, budget, |o| o.1)
        .into_iter()
        .map(|objects| {
            let mut buffer = Vec::new();
            let decompressed_size = compress_files(
                &mut buffer,
                objects.iter().map(|o| Ok(buffers[&o.0])),
                &CompressionOptions {
                    level: opts.compression_level,
                    window_log: opts.compression_window_log,
                    num_workers: 1,
                },
                &ProgressReporter::dummy(),
            )?;
            Ok(Frame {
                objects: objects.to_vec(),
                header: PackFrame {
                    frame_size: buffer.len() as u64,
                    decompressed_size,
                },
                offset: 0,
                buffer: Some(buffer),
            })
        })
        .collect()
}

impl Repository {
    /// Append loose snapshots, merging novel objects with their same-path
    /// predecessors (or the closest object-size range). Only affected frames
    /// are decoded and recompressed; overflowing frames are size-sorted and split.
    ///
    /// Existing snapshots and objects are retained. Duplicate tags are errors.
    /// Publication uses a recoverable pack/index transaction under the repository
    /// lock. Opening the repository finishes an interrupted publication.
    pub fn append_pack(
        &mut self,
        pack: &PackId,
        additions: PackIndex,
        opts: &PackOptions,
        reporter: &ProgressReporter,
    ) -> Result<(), Error> {
        opts.fill_size()?;
        if opts.num_frames != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--frames cannot be used with --append; use --frame-size and --frame-fill",
            )
            .into());
        }
        self.lock_exclusive()?;
        if self.is_pack_loose(pack) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot append to a loose snapshot",
            )
            .into());
        }
        let name = pack.to_string();
        transaction::validate_target(&name)?;
        let path = self.get_pack_path(pack);
        let mut previous = super::fs::open_file(&path)?;
        let mut index = self.load_index(pack)?;
        let mut frames = read_frames(&mut previous, &index)?;
        let mut last_seen = index.last_seen_objects()?;
        // Validate snapshot names and metadata before doing any codec work.
        index.append_snapshots(&additions)?;
        let mut home = frames
            .iter()
            .enumerate()
            .flat_map(|(i, f)| f.objects.iter().map(move |o| (o.0, i)))
            .collect::<HashMap<_, _>>();
        if frames.is_empty() {
            frames.push(Frame {
                objects: Vec::new(),
                header: PackFrame {
                    frame_size: 0,
                    decompressed_size: 0,
                },
                offset: 0,
                buffer: None,
            });
        }
        let mut assigned = vec![Vec::new(); frames.len()];
        additions.for_each_snapshot(|_, entries| {
            // Deterministic first-path affinity for a checksum shared by several paths.
            let mut entries = entries.iter().collect::<Vec<_>>();
            entries.sort_by(|a, b| a.path.cmp(b.path));
            for entry in entries {
                if !home.contains_key(entry.checksum) {
                    let frame = last_seen
                        .get(entry.path)
                        .and_then(|checksum| home.get(checksum))
                        .copied()
                        .unwrap_or_else(|| nearest_frame(&frames, entry.object_metadata.size));
                    assigned[frame].push((*entry.checksum, entry.object_metadata.size));
                    home.insert(*entry.checksum, frame);
                }
                last_seen.insert(entry.path.to_owned(), *entry.checksum);
            }
            ControlFlow::<()>::Continue(())
        })?;
        let jobs = assigned
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.is_empty())
            .collect::<Vec<_>>();
        log::info!(
            "Merging {} new objects into {} of {} frames",
            assigned.iter().map(Vec::len).sum::<usize>(),
            jobs.len(),
            frames.len()
        );
        let mut replacements = HashMap::new();
        // Bound simultaneously decoded frames by the requested worker count.
        for chunk in jobs.chunks(opts.num_workers as usize) {
            for result in
                run_in_parallel(opts.num_workers as usize, chunk.iter(), |(i, additions)| {
                    rewrite(&frames[*i], additions, &path, self, opts).map(|f| (*i, f))
                })
            {
                let (i, rewritten) = result?;
                replacements.insert(i, rewritten);
                reporter.checkpoint(replacements.len(), Some(jobs.len() - replacements.len()));
            }
        }
        let frames = frames
            .into_iter()
            .enumerate()
            .flat_map(|(i, f)| replacements.remove(&i).unwrap_or_else(|| vec![f]))
            .filter(|f| f.header.frame_size != 0)
            .collect::<Vec<_>>();
        let prepared = transaction::Prepared::new(self.data_dir())?;
        let mut writer = io::BufWriter::with_capacity(
            1024 * 1024,
            File::create(prepared.directory.join("new.pack"))?,
        );
        let header = PackHeader::new(frames.iter().map(|f| f.header.clone()).collect());
        write_skippable_frame(
            &mut writer,
            &rmp_serde::to_vec(&header).map_err(PackError::from)?,
        )?;
        let mut raw_offset = 0u64;
        let mut previous = io::BufReader::with_capacity(1024 * 1024, previous);
        for frame in frames {
            for (checksum, size) in frame.objects {
                index.set_object_offset(&checksum, raw_offset);
                raw_offset = raw_offset
                    .checked_add(size)
                    .filter(|n| *n != u64::MAX)
                    .ok_or(Error::CorruptPackIndex)?;
            }
            match frame.buffer {
                Some(buffer) => writer.write_all(&buffer)?,
                None => {
                    previous.seek(SeekFrom::Start(frame.offset))?;
                    if io::copy(
                        &mut previous.by_ref().take(frame.header.frame_size),
                        &mut writer,
                    )? != frame.header.frame_size
                    {
                        return Err(Error::CorruptPack);
                    }
                }
            }
        }
        writer.flush()?;
        drop(writer);
        drop(previous);
        index.save(prepared.directory.join("new.pack.idx"))?;
        prepared.commit(&name)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_keeps_objects_whole_and_leaves_headroom() {
        assert_eq!(
            partition_by_budget(&[0, 3, 4, 5, 20], 9, |n| *n),
            vec![&[0, 3, 4][..], &[5], &[20]]
        );
        assert_eq!(
            partition_by_budget(&[u64::MAX, 1], u64::MAX, |n| *n),
            vec![&[u64::MAX][..], &[1]]
        );
    }
}
