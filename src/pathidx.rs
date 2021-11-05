//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use core::fmt;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// PathPool interns path strings.
#[derive(Default, Clone)]
pub struct PathPool {
    entries: Vec<OsString>,
    entry_map: HashMap<OsString, PathHandle>,
}

pub type PathHandle = u32;

impl PathPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// get returns the intern'd PathHandle.
    pub fn get<P: AsRef<OsStr>>(&self, p: P) -> Option<PathHandle> {
        self.entry_map.get(p.as_ref()).map(|&x| x)
    }

    /// get_or_insert returns the intern'd PathHandle for the given string, or inserts it if not present.
    pub fn get_or_insert<P: AsRef<OsStr>>(&mut self, p: P) -> PathHandle {
        if let Some(h) = self.get(p.as_ref()) {
            h
        } else {
            let h = self.entries.len() as PathHandle;
            let p = p.as_ref().to_owned();
            self.entries.push(p.clone());
            self.entry_map.insert(p, h);
            h
        }
    }

    // lookup the string for the given PathHandle.
    pub fn lookup(&self, h: PathHandle) -> Option<&OsString> {
        self.entries.get(h as usize)
    }
}

use std::iter::FromIterator;
impl<'l> FromIterator<&'l OsString> for PathPool {
    fn from_iter<I: IntoIterator<Item = &'l OsString>>(iter: I) -> Self {
        let mut c = PathPool::new();
        for i in iter {
            c.get_or_insert(i);
        }
        c
    }
}

impl<'de> Deserialize<'de> for PathPool {
    fn deserialize<D>(deserializer: D) -> Result<PathPool, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VisitPathPool;
        impl<'de> Visitor<'de> for VisitPathPool {
            type Value = PathPool;
            fn visit_seq<V>(self, mut seq: V) -> Result<PathPool, V::Error>
            where
                V: SeqAccess<'de>,
            {
                let mut result = if let Some(len) = seq.size_hint() {
                    PathPool {
                        entries: Vec::with_capacity(len),
                        entry_map: HashMap::with_capacity(len),
                    }
                } else {
                    PathPool::new()
                };
                while let Some(elem) = seq.next_element::<OsString>()? {
                    result.get_or_insert(elem);
                }
                Ok(result)
            }
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("PathPool")
            }
        }

        deserializer.deserialize_seq(VisitPathPool)
    }
}

impl Serialize for PathPool {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(&self.entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_pathpool_test() {
        let mut p = PathPool::new();
        // Not present.
        let handle = p.get(&OsString::from("foo"));
        assert_eq!(handle, None);
        // Insertion.
        let first_handle = p.get_or_insert(&OsString::from("foo"));
        assert_eq!(p.lookup(first_handle), Some(&OsString::from("foo")));
        // Repeat insertion.
        let second_handle = p.get(&OsString::from("foo"));
        assert_eq!(Some(first_handle), second_handle);
        // Not present.
        let missing = p.get(&OsString::from("bar"));
        assert_eq!(missing, None);
        // Insertion but reusing a previously-was-file path fragment as a directory.
        let foobar = p.get_or_insert(&OsString::from("foo/bar"));
        assert_eq!(p.lookup(foobar), Some(&OsString::from("foo/bar")));
    }

    #[test]
    fn serde() {
        let paths: Vec<OsString> = vec!["a", "b", "c", "b", "a/a", "a/a/a", "a", "a/a"]
            .into_iter()
            .map(Into::into)
            .collect();

        let pool = PathPool::from_iter(paths.iter());
        let handles = paths
            .iter()
            .map(|p| pool.get(p).unwrap())
            .collect::<Vec<_>>();

        let mut encoded: Vec<u8> = vec![];
        rmp_serde::encode::write(&mut encoded, &pool).unwrap();

        let decoded: PathPool = rmp_serde::decode::from_read(&*encoded).unwrap();

        // Same handles given original paths.
        let decoded_handles = paths
            .iter()
            .map(|p| decoded.get(p).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(handles, decoded_handles);

        // Handles preserved across a roundtrip.
        for (path, orig_handle) in paths.iter().zip(handles.into_iter()) {
            assert_eq!(decoded.lookup(orig_handle), Some(path));
        }
    }
}
