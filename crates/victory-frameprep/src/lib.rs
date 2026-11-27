//! Preparing a frame on more than one thread, and measuring what that gives.
//!
//! A frame is prepared on the CPU before the card can draw it: what is
//! visible is found, commands are recorded, resources are made ready. This
//! crate has the parts of that which do not depend on a particular program:
//!
//! - [`sched`]: a worker pool of a chosen size whose every run says how long
//!   each worker was busy, how late it woke and how evenly the work fell, so
//!   that a thread count is chosen by measurement.
//! - [`record`]: a frame's items cut into parts, each recorded into a command
//!   list of its own by whichever worker takes it, the lists run in the
//!   items' order.
//! - [`descriptors`]: slots of a descriptor heap that is made once and kept.
//! - [`cull`]: what a view volume holds, found on the pool.
//! - [`profile`]: named CPU scopes, and the spacing of presented frames.
//! - [`d3d12`]: the Direct3D 12 backend: allocators and lists used again
//!   every frame, timestamps on the card, and indirect draws.
//! - [`bench`]: the same frame recorded with one worker up to all of them, on
//!   the machine at hand.
//!
//! What it does not do: make a program's own serial rendering code parallel.
//! A program records its frame in the order and on the threads it chose; a
//! layer under it can spread only its own work for that frame (translating,
//! refreshing, making ready), and only where that work is independent. More
//! threads are not faster by themselves: [`sched::RunStats`] shows when they
//! stop being so.
//!
//! Not here yet: a Vulkan backend ([`record::RecordBackend`] is what it will
//! implement), resource streaming in the background, and any wiring into the
//! engine in `victory-gpu`.

pub mod cull;
pub mod descriptors;
pub mod profile;
pub mod record;
pub mod sched;

#[cfg(windows)]
pub mod bench;
#[cfg(windows)]
pub mod d3d12;
