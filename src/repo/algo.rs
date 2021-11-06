//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

use std::panic;

/// run_in_parallel splits the input items into `nthread` even-sized groups, and
/// spawns one thread to handle each group. The `workload()` function is run on
/// each item to produce a Vec<Output> whose order matches that of the input.
pub fn run_in_parallel<Item, Output, Workload>(
    nthread: usize,
    items: impl ExactSizeIterator<Item = Item>,
    workload: Workload,
) -> Vec<Output>
where
    Workload: Copy + Send + Fn(Item) -> Output,
    Item: Send,
    Output: Send,
{
    assert!(!(nthread == 0), "nthread == 0");
    crossbeam_utils::thread::scope(|s| {
        let mut workers = Vec::new();
        let mut items = items.peekable();
        let n_per_thread = if nthread > items.len() {
            1
        } else {
            items.len() / nthread
        };
        while items.peek().is_some() {
            let thread_items = items.by_ref().take(n_per_thread).collect::<Vec<_>>();
            workers
                .push(s.spawn(move |_| thread_items.into_iter().map(workload).collect::<Vec<_>>()))
        }

        workers
            .into_iter()
            .map(|w| w.join())
            .collect::<Result<Vec<_>, _>>()
            .map(|v| v.into_iter().flatten().collect::<Vec<_>>())
    })
    // These errors only relate to thread failures, and contain panic information.
    // They don't pertain to any errors raised by the workload.
    .map_err(|payload| {
        panic::resume_unwind(payload);
    })
    .unwrap()
    .map_err(|payload| {
        panic::resume_unwind(payload);
    })
    .unwrap()
}

/// Partition the dataset into *up to* `n` partitions, using the values returned by `eval` as the partitioning metric.
pub fn partition_by_u64<T, F: Fn(&T) -> u64>(xs: &[T], n: u32, eval: F) -> Vec<&[T]> {
    if xs.is_empty() {
        return vec![];
    }
    // Limit n to number of items
    let n = std::cmp::min(n, xs.len() as u32);
    let total = xs.iter().fold(0, |s, x| s + eval(x));
    let bound = total / n as u64;

    let mut partitions: Vec<&[T]> = vec![];
    let mut i = 0;
    for _ in 0..(n - 1) {
        let begin = i;
        let mut curr = 0;
        while i < xs.len() {
            let v = eval(&xs[i]);
            if curr + v <= bound {
                curr += v;
            } else {
                break;
            }
            i += 1;
        }
        if begin != i {
            partitions.push(&xs[begin..i]);
        }
    }
    if i < xs.len() {
        partitions.push(&xs[i..]);
    }
    partitions
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn run_in_parallel_works() {
        let tasks = (0..10000000u64).collect::<Vec<_>>();
        let result = run_in_parallel(128, tasks.iter(), |&x| x * 2);
        let expected = tasks.into_iter().map(|x| x * 2).collect::<Vec<_>>();

        assert!(expected.eq(&result));
    }

    #[test]
    fn partition_by_u64_simple_works() {
        let partitions = partition_by_u64(&[1, 2, 3, 4, 5, 6, 7, 8, 9], 2, |&x| x as u64);
        assert_eq!(2, partitions.len());
        assert_eq!(&[1, 2, 3, 4, 5, 6], partitions[0]);
        assert_eq!(&[7, 8, 9], partitions[1]);
    }
    #[test]
    fn partition_by_u64_few_works() {
        let partitions = partition_by_u64(&[1, 2, 999], 4, |&x| x as u64);
        assert_eq!(2, partitions.len());
        assert_eq!(&[1, 2], partitions[0]);
        assert_eq!(&[999], partitions[1]);
    }
    #[test]
    fn partition_by_u64_small_values_works() {
        let partitions = partition_by_u64(&[1, 1, 1], 4, |&x| x as u64);
        assert_eq!(3, partitions.len());
        assert_eq!(&[1], partitions[0]);
        assert_eq!(&[1], partitions[1]);
        assert_eq!(&[1], partitions[2]);
    }
}
