//! Slots of a descriptor heap that is made once and kept.
//!
//! Making a descriptor heap, or writing every view a frame uses into a fresh
//! stretch of one, is CPU time spent again every frame. Here the heap stays:
//! a view that lives across frames keeps its slots, and only what is new
//! takes some. Runs of slots are handed out and taken back like ranges of
//! memory.

use std::sync::Mutex;

use victory_unimem::offset::OffsetAllocator;

/// The slots of one heap.
pub struct Slots {
    ranges: Mutex<OffsetAllocator>,
    capacity: u32,
}

impl Slots {
    pub fn new(capacity: u32) -> Slots {
        Slots { ranges: Mutex::new(OffsetAllocator::new(u64::from(capacity))), capacity }
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    pub fn used(&self) -> u32 {
        self.ranges.lock().unwrap_or_else(|e| e.into_inner()).used() as u32
    }

    /// `count` slots that follow one another (a descriptor table), or `None`
    /// when no such run is free.
    pub fn take(&self, count: u32) -> Option<u32> {
        self.ranges.lock().unwrap_or_else(|e| e.into_inner()).alloc(u64::from(count), 1).map(|first| first as u32)
    }

    /// Gives back a run [`Slots::take`] returned.
    pub fn give(&self, first: u32, count: u32) {
        self.ranges.lock().unwrap_or_else(|e| e.into_inner()).free(u64::from(first), u64::from(count));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_are_kept_until_given_back() {
        let slots = Slots::new(100);
        let a = slots.take(40).unwrap();
        let b = slots.take(40).unwrap();
        assert!(a + 40 <= b || b + 40 <= a);
        assert_eq!(slots.take(40), None, "only 20 are left");
        assert_eq!(slots.used(), 80);
        slots.give(a, 40);
        assert_eq!(slots.take(40), Some(a), "the run given back is used again");
        slots.give(a, 40);
        slots.give(b, 40);
        assert_eq!((slots.used(), slots.take(100)), (0, Some(0)));
    }
}
