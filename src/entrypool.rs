//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.
use std::fmt;
use std::marker::PhantomData;
use std::{borrow::Borrow, collections::HashMap, hash::Hash};

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

// An EntryPool interns sets of values and provides [`Handle`]-based access to
// them. Serializes to a single flat list with append semantics for newly-seen
// elements.
pub struct EntryPool<T> {
    entries: Vec<T>,
    // Provides hash-based lookup at runtime, but is not serialized to disk.
    entry_map: HashMap<T, Handle>,
}

pub type Handle = u32;

impl<T> EntryPool<T>
where
    T: Eq + Hash,
{
    pub fn new() -> Self {
        Self::default()
    }

    /// get returns the entry for the intern'd Handle, or None if absent.
    pub fn get<Q>(&self, k: &Q) -> Option<Handle>
    where
        T: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entry_map.get(k).copied()
    }

    /// get_or_insert returns the intern'd Handle for the given object, or
    /// inserts it if not present.
    pub fn get_or_insert<Q>(&mut self, p: &Q) -> Handle
    where
        T: Borrow<Q>,
        Q: ?Sized + Hash + Eq + ToOwned<Owned = T>,
    {
        if let Some(handle) = self.get(p) {
            handle
        } else {
            let handle = self.entries.len() as Handle;
            self.entry_map.insert(p.to_owned(), handle);
            self.entries.push(p.to_owned());
            handle
        }
    }

    /// lookup the entry for the given Handle.
    pub fn lookup(&self, h: Handle) -> Option<&T> {
        self.entries.get(h as usize)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> {
        self.entries.iter()
    }
}

impl<T> Default for EntryPool<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            entry_map: HashMap::new(),
        }
    }
}

use std::iter::FromIterator;
impl<'l, T, Q> FromIterator<&'l Q> for EntryPool<T>
where
    T: Borrow<Q> + Eq + Hash,
    Q: 'l + ?Sized + Hash + Eq + ToOwned<Owned = T>,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = &'l Q>,
    {
        let mut c = EntryPool::<T>::new();
        for i in iter {
            c.get_or_insert(i);
        }
        c
    }
}

impl<'de, T> Deserialize<'de> for EntryPool<T>
where
    T: Deserialize<'de> + Hash + Eq + Clone,
{
    fn deserialize<D>(deserializer: D) -> Result<EntryPool<T>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct VisitEntryPool<T>(PhantomData<T>);

        impl<'de, T> Visitor<'de> for VisitEntryPool<T>
        where
            T: Deserialize<'de> + Hash + Eq + Clone,
        {
            type Value = EntryPool<T>;

            fn visit_seq<V>(self, mut seq: V) -> Result<EntryPool<T>, V::Error>
            where
                V: SeqAccess<'de>,
            {
                let mut result = if let Some(len) = seq.size_hint() {
                    EntryPool {
                        entries: Vec::with_capacity(len),
                        entry_map: HashMap::with_capacity(len),
                    }
                } else {
                    EntryPool::<T>::new()
                };
                while let Some(elem) = seq.next_element::<T>()? {
                    result.get_or_insert(&elem);
                }
                Ok(result)
            }
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("PathPool")
            }
        }

        deserializer.deserialize_seq(VisitEntryPool(PhantomData::<T>))
    }
}

impl<T> Serialize for EntryPool<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_seq(&self.entries)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn basic_pathpool_test() {
        let mut p = EntryPool::new();
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

        let pool = EntryPool::from_iter(paths.iter());
        let handles = paths
            .iter()
            .map(|p| pool.get(p).unwrap())
            .collect::<Vec<_>>();

        let mut encoded: Vec<u8> = vec![];
        rmp_serde::encode::write(&mut encoded, &pool).unwrap();

        let decoded: EntryPool<OsString> = rmp_serde::decode::from_read(&*encoded).unwrap();

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
