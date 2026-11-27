//! The pool: one interface over the kinds of memory a [`Backend`] has.
//!
//! An allocation is a range of a block of one kind, chosen by
//! [`crate::policy::place`]. Writes go straight into mapped kinds; into card
//! memory that is not mapped they go through one staging block in system
//! memory and one copy the card runs, gathered until [`UnifiedPool::flush`].
//! Blocks nothing used for a while are evicted while the adapter's memory is
//! over its budget, and brought back before they are touched.
//!
//! All state is behind one lock, handles carry a generation so that a freed
//! one is refused, and every access is checked against its allocation's
//! length before a byte moves.

use std::sync::Mutex;

use crate::backend::{Backend, BackendError, CopyOp, HeapKind};
use crate::offset::OffsetAllocator;
use crate::policy::{Decision, Room, Usage, place};
use crate::stats::{KindStats, Snapshot, Traffic};

/// An allocation of a pool. Refused once it was freed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Handle {
    index: u32,
    generation: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("no such allocation (freed, or of another pool)")]
    Stale,
    #[error("{at}+{len} bytes lie outside an allocation of {size}")]
    OutOfRange { at: u64, len: u64, size: u64 },
    #[error("an allocation of no bytes")]
    Empty,
    #[error(transparent)]
    Backend(#[from] BackendError),
}

/// How a pool is run.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// The size of a block; larger allocations get a block of their own.
    pub block_size: u64,
    /// The staging block's size: the most staged bytes between two copies.
    pub staging_size: u64,
    /// The most bytes of card memory the CPU maps. Mapped card memory is the
    /// card's memory all the same: this is its share.
    pub mapped_vram_most: u64,
    /// The share of the adapter's budget, in percent, the pool fills before
    /// it spills into system memory and evicts.
    pub fill_percent: u64,
    /// Frames a block must have gone unused before it may be evicted.
    pub evict_after_frames: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config { block_size: 64 << 20, staging_size: 32 << 20, mapped_vram_most: 1 << 30, fill_percent: 90, evict_after_frames: 120 }
    }
}

struct Block<B> {
    block: B,
    kind: HeapKind,
    ranges: OffsetAllocator,
    last_used: u64,
    resident: bool,
    live: u32,
}

#[derive(Clone, Copy)]
struct Alloc {
    block: usize,
    at: u64,
    len: u64,
    usage: Usage,
    why: &'static str,
}

struct Slot {
    generation: u32,
    alloc: Option<Alloc>,
}

/// A staged write waiting for the card's copy.
struct Pending {
    staging_at: u64,
    block: usize,
    at: u64,
    len: u64,
}

struct State<B> {
    blocks: Vec<Option<Block<B>>>,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    staging: Option<B>,
    staging_used: u64,
    pending: Vec<Pending>,
    frame: u64,
    traffic: Traffic,
}

/// One logical pool over a backend's memory.
pub struct UnifiedPool<B: Backend> {
    backend: B,
    config: Config,
    state: Mutex<State<B::Block>>,
}

const ALIGN_BLOCK: u64 = 64 << 10;

impl<B: Backend> UnifiedPool<B> {
    pub fn new(backend: B, config: Config) -> Self {
        let state = State {
            blocks: Vec::new(),
            slots: Vec::new(),
            free_slots: Vec::new(),
            staging: None,
            staging_used: 0,
            pending: Vec::new(),
            frame: 0,
            traffic: Traffic::default(),
        };
        UnifiedPool { backend, config, state: Mutex::new(state) }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<B::Block>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Room for a new allocation: what is under the budget, and what the
    /// blocks already made still have free.
    fn room(&self, state: &State<B::Block>) -> Room {
        let budget = self.backend.budget();
        let line = budget.local_budget / 100 * self.config.fill_percent;
        let under = line.saturating_sub(budget.local_used);
        let free_in = |kind: HeapKind| {
            state.blocks.iter().flatten().filter(|b| b.kind == kind).map(|b| b.ranges.largest_free()).max().unwrap_or(0)
        };
        let mapped_reserved: u64 =
            state.blocks.iter().flatten().filter(|b| b.kind == HeapKind::MappedVram).map(|b| b.ranges.size()).sum();
        let mapped_left = self.config.mapped_vram_most.saturating_sub(mapped_reserved);
        Room {
            vram: under.max(free_in(HeapKind::Vram)),
            mapped_vram: under.min(mapped_left).max(free_in(HeapKind::MappedVram)),
        }
    }

    /// A range of `len` bytes in a block of `kind`: an existing block's, or a
    /// new block's.
    fn take(&self, state: &mut State<B::Block>, kind: HeapKind, len: u64, align: u64) -> Result<(usize, u64), BackendError> {
        for (i, block) in state.blocks.iter_mut().enumerate() {
            if let Some(block) = block.as_mut().filter(|b| b.kind == kind)
                && let Some(at) = block.ranges.alloc(len, align)
            {
                block.live += 1;
                return Ok((i, at));
            }
        }
        let size = len.max(self.config.block_size).next_multiple_of(ALIGN_BLOCK);
        let made = self.backend.create_block(kind, size)?;
        let mut ranges = OffsetAllocator::new(size);
        let at = ranges.alloc(len, align).expect("a new block holds what it was made for");
        let block = Block { block: made, kind, ranges, last_used: state.frame, resident: true, live: 1 };
        let i = match state.blocks.iter().position(Option::is_none) {
            Some(i) => {
                state.blocks[i] = Some(block);
                i
            }
            None => {
                state.blocks.push(Some(block));
                state.blocks.len() - 1
            }
        };
        Ok((i, at))
    }

    /// `len` bytes used as `usage`, starting on a multiple of `align` within
    /// their block (and so at the card's address, blocks being aligned to
    /// 64 KiB).
    pub fn allocate(&self, len: u64, align: u64, usage: Usage) -> Result<Handle, PoolError> {
        if len == 0 {
            return Err(PoolError::Empty);
        }
        let mut state = self.lock();
        let Decision { mut kind, mut why, .. } = place(usage, len, self.backend.caps(), self.room(&state));
        let (block, at) = match self.take(&mut state, kind, len, align) {
            Ok(taken) => taken,
            // The card would not give the memory after all: system memory,
            // which it reads across the bus.
            Err(BackendError::OutOfMemory(_) | BackendError::Unsupported(_)) if kind != HeapKind::SystemRam => {
                kind = HeapKind::SystemRam;
                why = "the adapter refused the memory asked for: system memory, across the bus";
                self.take(&mut state, kind, len, align)?
            }
            Err(e) => return Err(e.into()),
        };
        let alloc = Alloc { block, at, len, usage, why };
        let index = match state.free_slots.pop() {
            Some(index) => {
                state.slots[index as usize].alloc = Some(alloc);
                index
            }
            None => {
                state.slots.push(Slot { generation: 1, alloc: Some(alloc) });
                (state.slots.len() - 1) as u32
            }
        };
        Ok(Handle { index, generation: state.slots[index as usize].generation })
    }

    fn find(state: &State<B::Block>, handle: Handle) -> Result<Alloc, PoolError> {
        state
            .slots
            .get(handle.index as usize)
            .filter(|slot| slot.generation == handle.generation)
            .and_then(|slot| slot.alloc)
            .ok_or(PoolError::Stale)
    }

    fn check(alloc: &Alloc, at: u64, len: u64) -> Result<(), PoolError> {
        match at.checked_add(len) {
            Some(end) if end <= alloc.len => Ok(()),
            _ => Err(PoolError::OutOfRange { at, len, size: alloc.len }),
        }
    }

    /// The block is about to be used: counted as used now, and brought back
    /// if it was evicted.
    fn touch(&self, state: &mut State<B::Block>, block: usize) -> Result<(), PoolError> {
        let frame = state.frame;
        let b = state.blocks[block].as_mut().expect("a live allocation's block");
        b.last_used = frame;
        if !b.resident {
            self.backend.make_resident(&b.block)?;
            b.resident = true;
            state.traffic.made_resident += 1;
        }
        Ok(())
    }

    /// Gives an allocation back. Its block goes when nothing is left in it
    /// and a frame has ended.
    pub fn free(&self, handle: Handle) -> Result<(), PoolError> {
        let mut state = self.lock();
        let alloc = Self::find(&state, handle)?;
        // What was staged for it must not be copied into a range another
        // allocation may get.
        self.flush_locked(&mut state)?;
        let slot = &mut state.slots[handle.index as usize];
        slot.alloc = None;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        state.free_slots.push(handle.index);
        let block = state.blocks[alloc.block].as_mut().expect("a live allocation's block");
        block.ranges.free(alloc.at, alloc.len);
        block.live -= 1;
        Ok(())
    }

    /// Writes `data` at byte `at` of the allocation: straight into a mapped
    /// kind, staged for the card's copy otherwise.
    pub fn write(&self, handle: Handle, at: u64, data: &[u8]) -> Result<(), PoolError> {
        let mut state = self.lock();
        let alloc = Self::find(&state, handle)?;
        Self::check(&alloc, at, data.len() as u64)?;
        if data.is_empty() {
            return Ok(());
        }
        self.touch(&mut state, alloc.block)?;
        let block = state.blocks[alloc.block].as_ref().expect("a live allocation's block");
        if let Some(base) = self.backend.mapped(&block.block) {
            // SAFETY: the range was checked to lie in the allocation, which
            // lies in the block's mapping; the lock keeps the block alive and
            // `data` is not part of it.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), base.as_ptr().add((alloc.at + at) as usize), data.len()) };
            state.traffic.direct_bytes += data.len() as u64;
            return Ok(());
        }
        // Card memory that is not mapped: through staging, a piece at a time
        // if there is more than staging holds.
        let mut done = 0usize;
        while done < data.len() {
            if state.staging.is_none() {
                state.staging = Some(self.backend.create_block(HeapKind::SystemRam, self.config.staging_size)?);
            }
            if state.staging_used == self.config.staging_size {
                self.flush_locked(&mut state)?;
            }
            let piece = (data.len() - done).min((self.config.staging_size - state.staging_used) as usize);
            let staging = state.staging.as_ref().expect("made above");
            let base = self.backend.mapped(staging).ok_or(BackendError::Unsupported("mapped system memory"))?;
            // SAFETY: `piece` bytes fit behind `staging_used` in the staging
            // block's mapping, which the lock keeps alive.
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr().add(done), base.as_ptr().add(state.staging_used as usize), piece) };
            let pending = Pending { staging_at: state.staging_used, block: alloc.block, at: alloc.at + at + done as u64, len: piece as u64 };
            state.pending.push(pending);
            state.staging_used += piece as u64;
            done += piece;
        }
        Ok(())
    }

    fn flush_locked(&self, state: &mut State<B::Block>) -> Result<(), PoolError> {
        if state.pending.is_empty() {
            return Ok(());
        }
        let staging = state.staging.as_ref().expect("pending writes have a staging block");
        let ops: Vec<CopyOp<'_, B::Block>> = state
            .pending
            .iter()
            .map(|p| CopyOp {
                dst: &state.blocks[p.block].as_ref().expect("a staged write's block").block,
                dst_at: p.at,
                src: staging,
                src_at: p.staging_at,
                len: p.len,
            })
            .collect();
        self.backend.copy(&ops)?;
        drop(ops);
        state.traffic.staged_copies += state.pending.len() as u64;
        state.traffic.staged_bytes += state.pending.iter().map(|p| p.len).sum::<u64>();
        state.pending.clear();
        state.staging_used = 0;
        Ok(())
    }

    /// Runs the copies of the staged writes so far. Until then card memory
    /// that is not mapped holds what it held before them.
    pub fn flush(&self) -> Result<(), PoolError> {
        self.flush_locked(&mut self.lock())
    }

    /// Reads `out.len()` bytes at byte `at` of the allocation. In place from
    /// system and unified memory; from mapped card memory in place but slowly
    /// (counted); from card memory that is not mapped by a copy the card runs.
    pub fn read(&self, handle: Handle, at: u64, out: &mut [u8]) -> Result<(), PoolError> {
        let mut state = self.lock();
        let alloc = Self::find(&state, handle)?;
        Self::check(&alloc, at, out.len() as u64)?;
        if out.is_empty() {
            return Ok(());
        }
        self.touch(&mut state, alloc.block)?;
        let kind = state.blocks[alloc.block].as_ref().expect("a live allocation's block").kind;
        if kind == HeapKind::Vram {
            self.flush_locked(&mut state)?;
        }
        let block = state.blocks[alloc.block].as_ref().expect("a live allocation's block");
        match self.backend.mapped(&block.block) {
            Some(base) => {
                // SAFETY: as in `write`: a checked range of a live mapping.
                unsafe { std::ptr::copy_nonoverlapping(base.as_ptr().add((alloc.at + at) as usize), out.as_mut_ptr(), out.len()) };
                if kind == HeapKind::MappedVram {
                    state.traffic.slow_read_bytes += out.len() as u64;
                } else {
                    state.traffic.read_bytes += out.len() as u64;
                }
            }
            None => {
                self.backend.read_back(&block.block, alloc.at + at, out)?;
                state.traffic.readback_bytes += out.len() as u64;
            }
        }
        Ok(())
    }

    /// The card's address of the allocation's first byte, for binding it.
    pub fn gpu_address(&self, handle: Handle) -> Result<u64, PoolError> {
        let mut state = self.lock();
        let alloc = Self::find(&state, handle)?;
        self.touch(&mut state, alloc.block)?;
        let block = state.blocks[alloc.block].as_ref().expect("a live allocation's block");
        Ok(self.backend.gpu_address(&block.block) + alloc.at)
    }

    /// Which kind of memory holds the allocation, and why.
    pub fn placement(&self, handle: Handle) -> Result<(HeapKind, &'static str), PoolError> {
        let state = self.lock();
        let alloc = Self::find(&state, handle)?;
        Ok((state.blocks[alloc.block].as_ref().expect("a live allocation's block").kind, alloc.why))
    }

    /// Moves the allocation's bytes into memory of `kind`. The handle stays;
    /// the card's address changes.
    pub fn migrate(&self, handle: Handle, kind: HeapKind) -> Result<(), PoolError> {
        let mut state = self.lock();
        let old = Self::find(&state, handle)?;
        if state.blocks[old.block].as_ref().expect("a live allocation's block").kind == kind {
            return Ok(());
        }
        self.flush_locked(&mut state)?;
        self.touch(&mut state, old.block)?;
        let (block, at) = self.take(&mut state, kind, old.len, ALIGN_BLOCK.min(old.len.next_power_of_two()))?;
        let moved = {
            let src = &state.blocks[old.block].as_ref().expect("a live allocation's block").block;
            let dst = &state.blocks[block].as_ref().expect("made above").block;
            match (self.backend.mapped(src), self.backend.mapped(dst)) {
                (Some(from), Some(to)) => {
                    // SAFETY: two live mappings of different blocks, each
                    // holding the allocation's length at its offset.
                    unsafe { std::ptr::copy_nonoverlapping(from.as_ptr().add(old.at as usize), to.as_ptr().add(at as usize), old.len as usize) };
                    Ok(())
                }
                _ => self.backend.copy(&[CopyOp { dst, dst_at: at, src, src_at: old.at, len: old.len }]),
            }
        };
        if let Err(e) = moved {
            let b = state.blocks[block].as_mut().expect("made above");
            b.ranges.free(at, old.len);
            b.live -= 1;
            return Err(e.into());
        }
        let b = state.blocks[old.block].as_mut().expect("a live allocation's block");
        b.ranges.free(old.at, old.len);
        b.live -= 1;
        state.slots[handle.index as usize].alloc =
            Some(Alloc { block, at, why: "moved by the caller", ..old });
        state.traffic.migrated_bytes += old.len;
        Ok(())
    }

    /// A frame has ended: staged writes are copied, empty blocks go, and while
    /// the adapter's memory is over its line the blocks longest unused are
    /// evicted.
    pub fn end_frame(&self) -> Result<(), PoolError> {
        let mut state = self.lock();
        self.flush_locked(&mut state)?;
        state.frame += 1;
        let frame = state.frame;
        for slot in &mut state.blocks {
            if slot.as_ref().is_some_and(|b| b.live == 0 && frame - b.last_used >= 2) {
                *slot = None;
            }
        }
        let budget = self.backend.budget();
        let line = budget.local_budget / 100 * self.config.fill_percent;
        let mut over = budget.local_used.saturating_sub(line);
        if over == 0 {
            return Ok(());
        }
        let mut idle: Vec<usize> = (0..state.blocks.len())
            .filter(|&i| {
                state.blocks[i].as_ref().is_some_and(|b| {
                    b.resident && matches!(b.kind, HeapKind::Vram | HeapKind::MappedVram) && frame - b.last_used >= self.config.evict_after_frames
                })
            })
            .collect();
        idle.sort_by_key(|&i| state.blocks[i].as_ref().map_or(0, |b| b.last_used));
        for i in idle {
            if over == 0 {
                break;
            }
            let b = state.blocks[i].as_mut().expect("filtered above");
            self.backend.evict(&b.block)?;
            b.resident = false;
            over = over.saturating_sub(b.ranges.size());
            state.traffic.evictions += 1;
        }
        Ok(())
    }

    /// What the pool holds and moved, now.
    pub fn snapshot(&self) -> Snapshot {
        let state = self.lock();
        let mut kinds = [KindStats::default(); 4];
        for b in state.blocks.iter().flatten() {
            let k = &mut kinds[b.kind.index()];
            k.blocks += 1;
            k.reserved += b.ranges.size();
            k.used += b.ranges.used();
            k.allocations += u64::from(b.live);
            k.evicted_blocks += u64::from(!b.resident);
        }
        Snapshot { frame: state.frame, kinds, traffic: state.traffic, budget: self.backend.budget() }
    }

    /// The usage an allocation was made with.
    pub fn usage(&self, handle: Handle) -> Result<Usage, PoolError> {
        Ok(Self::find(&self.lock(), handle)?.usage)
    }
}

#[cfg(test)]
mod tests;
