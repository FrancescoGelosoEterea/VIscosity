//! Ranges handed out of one block: best fit, split at both ends for
//! alignment, joined with their neighbours when given back.

use std::collections::{BTreeMap, BTreeSet};

/// The free ranges of a block of `size` bytes.
#[derive(Debug)]
pub struct OffsetAllocator {
    size: u64,
    /// Free ranges by where they start: start -> length.
    by_start: BTreeMap<u64, u64>,
    /// The same ranges by length, for the smallest that fits.
    by_len: BTreeSet<(u64, u64)>,
    used: u64,
}

impl OffsetAllocator {
    pub fn new(size: u64) -> Self {
        let mut all = OffsetAllocator { size, by_start: BTreeMap::new(), by_len: BTreeSet::new(), used: 0 };
        if size > 0 {
            all.insert(0, size);
        }
        all
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn used(&self) -> u64 {
        self.used
    }

    /// The longest range that is free.
    pub fn largest_free(&self) -> u64 {
        self.by_len.last().map_or(0, |&(len, _)| len)
    }

    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    fn insert(&mut self, start: u64, len: u64) {
        self.by_start.insert(start, len);
        self.by_len.insert((len, start));
    }

    fn remove(&mut self, start: u64, len: u64) {
        self.by_start.remove(&start);
        self.by_len.remove(&(len, start));
    }

    /// `len` bytes starting on a multiple of `align` (a power of two; 0 and 1
    /// mean any). `None` if no free range holds them.
    pub fn alloc(&mut self, len: u64, align: u64) -> Option<u64> {
        if len == 0 {
            return None;
        }
        let align = align.max(1);
        debug_assert!(align.is_power_of_two());
        // The smallest free range that still holds the bytes once its start
        // is rounded up.
        let (start, free, at) = self.by_len.range((len, 0)..).find_map(|&(free, start)| {
            let at = start.checked_add(align - 1)? & !(align - 1);
            (at.checked_add(len)? <= start + free).then_some((start, free, at))
        })?;
        self.remove(start, free);
        if at > start {
            self.insert(start, at - start);
        }
        let end = at + len;
        if end < start + free {
            self.insert(end, start + free - end);
        }
        self.used += len;
        Some(at)
    }

    /// Gives back a range [`OffsetAllocator::alloc`] returned.
    pub fn free(&mut self, at: u64, len: u64) {
        debug_assert!(len > 0 && at + len <= self.size);
        let (mut start, mut end) = (at, at + len);
        if let Some((&before, &before_len)) = self.by_start.range(..at).next_back() {
            debug_assert!(before + before_len <= at, "a range freed twice");
            if before + before_len == at {
                self.remove(before, before_len);
                start = before;
            }
        }
        if let Some(&after_len) = self.by_start.get(&end) {
            self.remove(end, after_len);
            end += after_len;
        }
        self.insert(start, end - start);
        self.used -= len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_are_aligned_and_come_back_as_one() {
        let mut a = OffsetAllocator::new(1 << 20);
        let x = a.alloc(100, 1).unwrap();
        let y = a.alloc(4096, 4096).unwrap();
        let z = a.alloc(65536, 65536).unwrap();
        assert_eq!((x, y % 4096, z % 65536), (0, 0, 0));
        assert_eq!(a.used(), 100 + 4096 + 65536);
        a.free(y, 4096);
        a.free(x, 100);
        a.free(z, 65536);
        assert!(a.is_empty());
        assert_eq!(a.largest_free(), 1 << 20, "the pieces joined again");
    }

    #[test]
    fn a_full_block_gives_nothing_and_the_best_fit_is_taken() {
        let mut a = OffsetAllocator::new(1000);
        let big = a.alloc(600, 1).unwrap();
        let small = a.alloc(100, 1).unwrap();
        assert!(a.alloc(400, 1).is_none());
        a.free(big, 600);
        // 600 free at the front, 300 at the back: 250 goes into the back.
        let fit = a.alloc(250, 1).unwrap();
        assert!(fit >= small + 100);
        assert_eq!(a.alloc(0, 1), None);
    }

    /// Many random takes and returns: no two live ranges overlap, each lies
    /// in the block, and the count of used bytes is the sum of the live ones.
    #[test]
    fn random_use_keeps_the_ranges_apart() {
        let mut a = OffsetAllocator::new(1 << 24);
        let mut live: Vec<(u64, u64)> = Vec::new();
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            if live.len() > 300 || (next() % 3 == 0 && !live.is_empty()) {
                let (at, len) = live.swap_remove((next() % live.len() as u64) as usize);
                a.free(at, len);
            } else {
                let len = 1 + next() % 100_000;
                let align = 1u64 << (next() % 13);
                if let Some(at) = a.alloc(len, align) {
                    assert_eq!(at % align, 0);
                    assert!(at + len <= a.size());
                    assert!(live.iter().all(|&(b, n)| at + len <= b || b + n <= at), "overlap");
                    live.push((at, len));
                }
            }
            assert_eq!(a.used(), live.iter().map(|r| r.1).sum::<u64>());
        }
        for (at, len) in live {
            a.free(at, len);
        }
        assert_eq!(a.largest_free(), 1 << 24);
    }
}
