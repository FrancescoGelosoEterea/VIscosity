//! The pool against a backend made of plain memory: the logic, on any
//! machine. (`crate::d3d12` tests the same calls on a real device.)

use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use super::*;
use crate::backend::Budget;
use crate::policy::Caps;

struct Mock {
    caps: Caps,
    /// The system's budget of card memory, which it may change.
    budget: AtomicU64,
    /// Bytes of card memory alive, copies run, bytes read back.
    used: Arc<AtomicU64>,
    copies: AtomicU64,
}

struct MockBlock {
    kind: HeapKind,
    memory: NonNull<u8>,
    len: usize,
    resident: AtomicBool,
    used: Arc<AtomicU64>,
}

// SAFETY: plain memory of its own, reached only under the pool's lock.
unsafe impl Send for MockBlock {}
// SAFETY: as above.
unsafe impl Sync for MockBlock {}

/// Blocks begin on 64 KiB, as a device's do.
fn layout(len: usize) -> std::alloc::Layout {
    std::alloc::Layout::from_size_align(len, 64 << 10).unwrap()
}

impl Drop for MockBlock {
    fn drop(&mut self) {
        // SAFETY: the allocation made in `create_block`, given back once.
        unsafe { std::alloc::dealloc(self.memory.as_ptr(), layout(self.len)) };
        if matches!(self.kind, HeapKind::Vram | HeapKind::MappedVram) {
            self.used.fetch_sub(self.len as u64, Relaxed);
        }
    }
}

impl Backend for Mock {
    type Block = MockBlock;

    fn caps(&self) -> Caps {
        self.caps
    }

    fn budget(&self) -> Budget {
        Budget { local_budget: self.budget.load(Relaxed), local_used: self.used.load(Relaxed), shared_budget: 0, shared_used: 0 }
    }

    fn create_block(&self, kind: HeapKind, size: u64) -> Result<MockBlock, BackendError> {
        if kind == HeapKind::MappedVram && !self.caps.mapped_vram {
            return Err(BackendError::Unsupported("card memory the CPU maps"));
        }
        // SAFETY: a layout of `size` bytes, more than none.
        let memory = NonNull::new(unsafe { std::alloc::alloc_zeroed(layout(size as usize)) }).ok_or(BackendError::OutOfMemory("plain memory"))?;
        if matches!(kind, HeapKind::Vram | HeapKind::MappedVram) {
            self.used.fetch_add(size, Relaxed);
        }
        Ok(MockBlock {
            kind,
            memory,
            len: size as usize,
            resident: AtomicBool::new(true),
            used: self.used.clone(),
        })
    }

    fn mapped(&self, block: &MockBlock) -> Option<NonNull<u8>> {
        block.kind.mapped().then_some(block.memory)
    }

    fn gpu_address(&self, block: &MockBlock) -> u64 {
        block.memory.as_ptr() as u64
    }

    fn copy(&self, ops: &[CopyOp<'_, MockBlock>]) -> Result<(), BackendError> {
        for op in ops {
            assert!(op.dst.resident.load(Relaxed) && op.src.resident.load(Relaxed), "a copy touches evicted memory");
            assert!(op.dst_at + op.len <= op.dst.len as u64 && op.src_at + op.len <= op.src.len as u64);
            // SAFETY: ranges checked just above, in two live blocks.
            unsafe { std::ptr::copy(op.src.memory.as_ptr().add(op.src_at as usize), op.dst.memory.as_ptr().add(op.dst_at as usize), op.len as usize) };
            self.copies.fetch_add(1, Relaxed);
        }
        Ok(())
    }

    fn read_back(&self, block: &MockBlock, at: u64, out: &mut [u8]) -> Result<(), BackendError> {
        assert!(block.resident.load(Relaxed), "a read-back touches evicted memory");
        // SAFETY: the pool checked the range against the allocation.
        unsafe { std::ptr::copy_nonoverlapping(block.memory.as_ptr().add(at as usize), out.as_mut_ptr(), out.len()) };
        Ok(())
    }

    fn evict(&self, block: &MockBlock) -> Result<(), BackendError> {
        block.resident.store(false, Relaxed);
        Ok(())
    }

    fn make_resident(&self, block: &MockBlock) -> Result<(), BackendError> {
        block.resident.store(true, Relaxed);
        Ok(())
    }
}

fn pool(caps: Caps, budget: u64) -> UnifiedPool<Mock> {
    let mock = Mock { caps, budget: AtomicU64::new(budget), used: Arc::default(), copies: AtomicU64::new(0) };
    let config = Config { block_size: 1 << 20, staging_size: 256 << 10, mapped_vram_most: 4 << 20, fill_percent: 90, evict_after_frames: 3 };
    UnifiedPool::new(mock, config)
}

const DISCRETE: Caps = Caps { unified: false, mapped_vram: true };

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

#[test]
fn bytes_written_come_back_from_every_kind() {
    let pool = pool(DISCRETE, 64 << 20);
    for (usage, kind) in [
        (Usage::STATIC, HeapKind::Vram),
        (Usage::DYNAMIC, HeapKind::MappedVram),
        (Usage::CPU_SIDE, HeapKind::SystemRam),
        (Usage::GPU_OUTPUT, HeapKind::Vram),
    ] {
        let h = pool.allocate(300_000, 256, usage).unwrap();
        assert_eq!(pool.placement(h).unwrap().0, kind, "{usage:?}");
        let data = pattern(300_000, kind as u8);
        pool.write(h, 0, &data).unwrap();
        pool.write(h, 1000, &data[..500]).unwrap();
        let mut back = vec![0u8; 300_000];
        pool.read(h, 0, &mut back).unwrap();
        assert_eq!(back[..1000], data[..1000]);
        assert_eq!(back[1000..1500], data[..500]);
        assert_eq!(back[1500..], data[1500..]);
        assert_eq!(pool.gpu_address(h).unwrap() % 256, 0);
    }
    let s = pool.snapshot();
    // Two allocations in card memory were written through staging (more
    // than staging holds at once: several copies each), two directly.
    assert_eq!(s.traffic.staged_bytes, 2 * 300_500);
    assert_eq!(s.traffic.direct_bytes, 2 * 300_500);
    assert!(s.traffic.staged_copies >= 4);
    assert_eq!(s.traffic.slow_read_bytes, 300_000, "the read of mapped card memory is counted as slow");
    assert_eq!(s.traffic.readback_bytes, 2 * 300_000);
}

#[test]
fn a_unified_adapter_copies_nothing() {
    let pool = pool(Caps { unified: true, mapped_vram: false }, 64 << 20);
    let h = pool.allocate(100_000, 64, Usage::STATIC).unwrap();
    pool.write(h, 0, &pattern(100_000, 7)).unwrap();
    pool.end_frame().unwrap();
    let s = pool.snapshot();
    assert_eq!(pool.placement(h).unwrap().0, HeapKind::Unified);
    assert_eq!((s.traffic.staged_bytes, s.traffic.direct_bytes), (0, 100_000));
    assert_eq!(pool.backend().copies.load(Relaxed), 0);
}

#[test]
fn stale_handles_and_ranges_outside_are_refused() {
    let pool = pool(DISCRETE, 64 << 20);
    let h = pool.allocate(1000, 16, Usage::DYNAMIC).unwrap();
    assert!(matches!(pool.write(h, 990, &[0; 11]), Err(PoolError::OutOfRange { .. })));
    assert!(matches!(pool.read(h, u64::MAX, &mut [0; 2]), Err(PoolError::OutOfRange { .. })));
    assert!(matches!(pool.allocate(0, 1, Usage::STATIC), Err(PoolError::Empty)));
    pool.free(h).unwrap();
    assert!(matches!(pool.write(h, 0, &[1]), Err(PoolError::Stale)));
    assert!(matches!(pool.free(h), Err(PoolError::Stale)));
    // The slot is used again under another generation.
    let again = pool.allocate(1000, 16, Usage::DYNAMIC).unwrap();
    assert_ne!(again, h);
    assert!(matches!(pool.read(h, 0, &mut [0]), Err(PoolError::Stale)));
}

#[test]
fn a_full_card_spills_and_idle_blocks_are_evicted_and_come_back() {
    // A budget of 4 MiB, blocks of 1 MiB: the line is at 3.6 MiB.
    let pool = pool(Caps { unified: false, mapped_vram: false }, 4 << 20);
    let data = pattern(900_000, 3);
    let handles: Vec<Handle> = (0..6).map(|_| pool.allocate(900_000, 64, Usage::STATIC).unwrap()).collect();
    for &h in &handles {
        pool.write(h, 0, &data).unwrap();
    }
    let kinds: Vec<HeapKind> = handles.iter().map(|&h| pool.placement(h).unwrap().0).collect();
    // Three blocks are 3 MiB; a fourth would pass the line.
    assert_eq!(kinds, [HeapKind::Vram, HeapKind::Vram, HeapKind::Vram, HeapKind::SystemRam, HeapKind::SystemRam, HeapKind::SystemRam]);
    // Nothing is evicted while the card is under its line, however idle.
    for _ in 0..5 {
        pool.end_frame().unwrap();
    }
    assert_eq!(pool.snapshot().traffic.evictions, 0);
    // The system takes half the budget away (another program wants the
    // card): over the line now, and after three frames of nobody using the
    // first blocks the longest unused go, not the one in use.
    pool.backend().budget.store(2 << 20, Relaxed);
    for _ in 0..4 {
        pool.read(handles[2], 0, &mut [0; 8]).unwrap();
        pool.end_frame().unwrap();
    }
    let s = pool.snapshot();
    assert_eq!((s.traffic.evictions, s.kind(HeapKind::Vram).evicted_blocks), (2, 2), "{s}");
    // An evicted block is brought back before its bytes are read.
    let mut back = vec![0u8; 900_000];
    pool.read(handles[0], 0, &mut back).unwrap();
    assert_eq!(back, data);
    assert!(pool.snapshot().traffic.made_resident >= 1);
}

#[test]
fn an_allocation_moves_between_kinds_with_its_bytes() {
    let pool = pool(DISCRETE, 64 << 20);
    let data = pattern(200_000, 9);
    let h = pool.allocate(200_000, 64, Usage::STATIC).unwrap();
    pool.write(h, 0, &data).unwrap();
    for kind in [HeapKind::SystemRam, HeapKind::MappedVram, HeapKind::Vram, HeapKind::SystemRam] {
        pool.migrate(h, kind).unwrap();
        assert_eq!(pool.placement(h).unwrap().0, kind);
        let mut back = vec![0u8; 200_000];
        pool.read(h, 0, &mut back).unwrap();
        assert_eq!(back, data, "after moving to {kind:?}");
    }
    assert_eq!(pool.snapshot().traffic.migrated_bytes, 4 * 200_000);
    // Blocks left empty go after two frames.
    pool.end_frame().unwrap();
    pool.end_frame().unwrap();
    pool.end_frame().unwrap();
    let s = pool.snapshot();
    assert_eq!(s.kind(HeapKind::Vram).blocks + s.kind(HeapKind::MappedVram).blocks, 0, "{s}");
}

#[test]
fn many_threads_share_a_pool() {
    let pool = Arc::new(pool(DISCRETE, 256 << 20));
    let threads: Vec<_> = (0..8u8)
        .map(|t| {
            let pool = pool.clone();
            std::thread::spawn(move || {
                for round in 0..40u8 {
                    let usage = [Usage::STATIC, Usage::DYNAMIC, Usage::CPU_SIDE][usize::from(round % 3)];
                    let len = 1000 + usize::from(round) * 997;
                    let h = pool.allocate(len as u64, 16, usage).unwrap();
                    let data = pattern(len, t.wrapping_mul(17).wrapping_add(round));
                    pool.write(h, 0, &data).unwrap();
                    let mut back = vec![0u8; len];
                    pool.read(h, 0, &mut back).unwrap();
                    assert_eq!(back, data);
                    if round % 2 == 0 {
                        pool.free(h).unwrap();
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert_eq!(pool.snapshot().kinds.iter().map(|k| k.allocations).sum::<u64>(), 8 * 20);
}
