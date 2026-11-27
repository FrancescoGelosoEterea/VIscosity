//! What a graphics API must offer for the pool to manage its memory: blocks
//! of each real kind of memory, a CPU address for those that have one, copies
//! the card runs, residency, and the system's budget.

use std::ptr::NonNull;

/// A real kind of memory. Nothing here is simulated: each is a heap type of
/// the API, with the properties the hardware gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HeapKind {
    /// The card's own memory, not mapped. The CPU writes it through a staging
    /// copy and reads it through a read-back copy.
    Vram,
    /// The card's own memory, mapped for the CPU (needs a resizable BAR and a
    /// driver that offers it). Written directly; CPU reads are slow.
    MappedVram,
    /// The PC's memory, mapped. On a discrete adapter the card reads it across
    /// the bus: never the same bandwidth as its own memory.
    SystemRam,
    /// Memory both sides use in place, on an adapter that shares the PC's
    /// memory (and only there).
    Unified,
}

impl HeapKind {
    pub const ALL: [HeapKind; 4] = [HeapKind::Vram, HeapKind::MappedVram, HeapKind::SystemRam, HeapKind::Unified];

    /// Whether the CPU has an address for it.
    pub fn mapped(self) -> bool {
        self != HeapKind::Vram
    }

    pub fn name(self) -> &'static str {
        match self {
            HeapKind::Vram => "card memory",
            HeapKind::MappedVram => "card memory the CPU maps",
            HeapKind::SystemRam => "system memory",
            HeapKind::Unified => "unified memory",
        }
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }
}

/// What the system lets this process use now, in bytes. `local` is the
/// adapter's own memory, `shared` the PC's memory it may use as well.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    pub local_budget: u64,
    pub local_used: u64,
    pub shared_budget: u64,
    pub shared_used: u64,
}

impl Budget {
    /// Bytes of the adapter's own memory still under the budget.
    pub fn local_free(&self) -> u64 {
        self.local_budget.saturating_sub(self.local_used)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("the adapter does not offer {0}")]
    Unsupported(&'static str),
    #[error("the device refused: {0}")]
    Device(String),
    #[error("out of {0}")]
    OutOfMemory(&'static str),
}

/// One copy the card runs: `len` bytes from `src` at `src_at` to `dst` at
/// `dst_at`.
pub struct CopyOp<'a, B> {
    pub dst: &'a B,
    pub dst_at: u64,
    pub src: &'a B,
    pub src_at: u64,
    pub len: u64,
}

/// A graphics API's memory, as the pool uses it.
pub trait Backend: Send + Sync {
    /// A block of one kind of memory: a buffer the size asked for.
    type Block: Send + Sync;

    /// Which kinds the adapter has.
    fn caps(&self) -> crate::policy::Caps;

    /// The system's budget now.
    fn budget(&self) -> Budget;

    /// A block of `size` bytes of `kind`, resident.
    fn create_block(&self, kind: HeapKind, size: u64) -> Result<Self::Block, BackendError>;

    /// The CPU's address of a mapped block's first byte; `None` for
    /// [`HeapKind::Vram`]. Valid while the block lives.
    fn mapped(&self, block: &Self::Block) -> Option<NonNull<u8>>;

    /// The card's address of the block's first byte.
    fn gpu_address(&self, block: &Self::Block) -> u64;

    /// Runs the copies on the card and returns when they are done.
    fn copy(&self, ops: &[CopyOp<'_, Self::Block>]) -> Result<(), BackendError>;

    /// Reads `out.len()` bytes at `at` of a block that is not mapped, through
    /// a copy the card runs.
    fn read_back(&self, block: &Self::Block, at: u64, out: &mut [u8]) -> Result<(), BackendError>;

    /// Lets the system take the block's memory; its contents stay.
    fn evict(&self, block: &Self::Block) -> Result<(), BackendError>;

    /// Brings an evicted block's memory back before it is used.
    fn make_resident(&self, block: &Self::Block) -> Result<(), BackendError>;
}
