//! One logical pool over the PC's memory and the graphics card's.
//!
//! A console has one memory: what the CPU writes the graphics processor
//! reads in place. A PC with a card of its own has two, joined by a bus. This
//! crate does not pretend otherwise. It gives one allocation interface and
//! decides, for each allocation, which of the real kinds of memory the
//! hardware offers should hold it, so that as few bytes as possible cross the
//! bus and none cross it twice:
//!
//! - [`HeapKind::Vram`]: the card's own memory. Fast for the card; the CPU
//!   reaches it only by a copy the card runs.
//! - [`HeapKind::MappedVram`]: the card's memory mapped into the CPU's address
//!   space (a Direct3D 12 GPU upload heap, which needs a resizable BAR). The
//!   CPU writes it directly, with no second copy; reading it back on the CPU
//!   is slow, and [`UnifiedPool::read`] counts every such read.
//! - [`HeapKind::SystemRam`]: the PC's memory, which the card reads across the
//!   bus. Fast for the CPU, slow for the card on a discrete adapter.
//! - [`HeapKind::Unified`]: on an adapter whose memory *is* the PC's (an
//!   integrated one), a heap both sides use in place.
//!
//! The parts: [`hw`] finds out what the machine has, [`policy`] decides a
//! placement from how an allocation is used, [`offset`] carves allocations out
//! of large blocks, [`UnifiedPool`] ties them to a [`Backend`], and [`stats`]
//! counts what moved. [`d3d12`] is the Direct3D 12 backend; [`bench`] measures
//! the bandwidth of each kind on the machine it runs on.
//!
//! Not here yet: a Vulkan backend (the [`Backend`] trait is what it will
//! implement), textures (blocks are buffers), and moving an allocation from
//! one kind to another by itself when pressure changes.

pub mod backend;
#[cfg(windows)]
pub mod bench;
#[cfg(windows)]
pub mod d3d12;
pub mod hw;
pub mod offset;
pub mod policy;
pub mod pool;
pub mod stats;

pub use backend::{Backend, BackendError, Budget, HeapKind};
pub use policy::{Caps, Decision, Rate, Usage};
pub use pool::{Handle, PoolError, UnifiedPool};
pub use stats::Snapshot;
