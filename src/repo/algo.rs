//! SPDX-License-Identifier: Apache-2.0
//! Copyright (C) 2021 Arm Limited or its affiliates and Contributors. All rights reserved.

unsafe fn extend_lifetime<'a, F, R>(f: F) -> Box<dyn FnOnce() -> R + Send + 'static>
where
    F: FnOnce() -> R + Send + 'a,
{
    std::mem::transmute::<Box<dyn FnOnce() -> R + Send + 'a>, Box<dyn FnOnce() -> R + Send + 'static>>(
        Box::new(f),
    )
}

pub fn run_in_parallel<T, R>(
    tasks: impl Iterator<Item = T>,
    num_threads: u32,
) -> impl Iterator<Item = R>
where
    T: Send + FnOnce() -> R,
    R: Send + 'static,
{
    assert_ne!(0, num_threads);
    let queue: std::collections::VecDeque<_> = tasks
        .enumerate()
        // It is safe to extend the lifetime here because we know that
        // we always join all threads before this function exists
        .map(|(i, t)| unsafe { (i, extend_lifetime(t)) })
        .collect();
    let queue = std::sync::Arc::new(std::sync::Mutex::new(queue));
    let threads: Vec<_> = (0..num_threads)
        .map(|_| {
            let queue = queue.clone();
            std::thread::spawn(move || {
                let mut results = vec![];
                loop {
                    let item = {
                        let mut queue = queue.lock().expect("Poisoned mutex!");
                        queue.pop_front()
                    };
                    if let Some((i, item)) = item {
                        results.push((i, item()));
                    } else {
                        break;
                    }
                }
                results
            })
        })
        .collect();

    let mut results = threads
        .into_iter()
        .map(|t| t.join())
        // Join all threads before proceeding. This has the implication that a panic in a thread
        // will require all threads to exit before it is propagated to the caller.
        .collect::<Vec<_>>()
        .into_iter()
        // Unwrap the thread results
        // This panics only if the thread panicked,
        // and returns the task result otherwise.
        .flat_map(|j| j.unwrap())
        .collect::<Vec<_>>();

    // Sort the results in the original task order.
    results.sort_by_key(|(i, _)| *i);
    // And return an iterator.
    results.into_iter().map(|(_, r)| r)
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
        let tasks = (0..8192).map(|n| move || n * 2);
        let expected = (0..8192).map(|n| n * 2);
        let result = run_in_parallel(tasks, 64);
        assert!(expected.eq(result));
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
