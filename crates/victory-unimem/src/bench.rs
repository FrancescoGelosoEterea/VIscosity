//! How fast bytes move on this machine: within the PC's memory, into and out
//! of each kind of memory the adapter has, and between them on the card.
//!
//! These are the numbers placement goes by, measured and not assumed: the
//! best of several passes over a buffer, each pass timed whole, copies by the
//! card with the wait for the card included.

use std::time::Instant;

use crate::backend::{Backend, CopyOp, HeapKind};
use crate::d3d12::D3d12;

/// One measurement.
#[derive(Clone, Debug)]
pub struct Measure {
    pub what: String,
    pub bytes: u64,
    /// Megabytes a second, the best pass.
    pub mb_per_s: f64,
}

fn best(passes: u32, bytes: u64, mut pass: impl FnMut()) -> f64 {
    let mut least = f64::MAX;
    for _ in 0..passes {
        let started = Instant::now();
        pass();
        least = least.min(started.elapsed().as_secs_f64());
    }
    bytes as f64 / (1u64 << 20) as f64 / least.max(1e-9)
}

/// Measures with buffers of `size` bytes (reads of mapped card memory use a
/// sixteenth of it: they are slow).
pub fn run(backend: &D3d12, size: u64) -> Vec<Measure> {
    let mut out = Vec::new();
    let n = size as usize;
    let source: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(131).wrapping_add(7)).collect();
    let mut sink = vec![0u8; n];
    let mut add = |what: &str, bytes: u64, mb_per_s: f64| out.push(Measure { what: what.to_owned(), bytes, mb_per_s });

    add("CPU copy within system memory (plain memory, the yardstick)", size, best(5, size, || {
        sink.copy_from_slice(std::hint::black_box(&source));
    }));

    let caps = backend.caps();
    let kinds: Vec<HeapKind> = [HeapKind::SystemRam, HeapKind::MappedVram, HeapKind::Unified]
        .into_iter()
        .filter(|k| match k {
            HeapKind::MappedVram => caps.mapped_vram,
            HeapKind::Unified => caps.unified,
            _ => true,
        })
        .collect();
    let vram = (!caps.unified).then(|| backend.create_block(HeapKind::Vram, size)).and_then(Result::ok);
    for kind in kinds {
        let Ok(block) = backend.create_block(kind, size) else {
            continue;
        };
        let Some(base) = backend.mapped(&block) else {
            continue;
        };
        add(&format!("CPU writes into {}", kind.name()), size, best(5, size, || {
            // SAFETY: the block's own mapping, `size` bytes long.
            unsafe { std::ptr::copy_nonoverlapping(source.as_ptr(), base.as_ptr(), n) };
        }));
        let read = if kind == HeapKind::MappedVram { n / 16 } else { n };
        add(&format!("CPU reads from {}", kind.name()), read as u64, best(3, read as u64, || {
            // SAFETY: as above, the first `read` bytes.
            unsafe { std::ptr::copy_nonoverlapping(base.as_ptr(), sink.as_mut_ptr(), read) };
            std::hint::black_box(&sink);
        }));
        if let Some(vram) = &vram {
            let ops = [CopyOp { dst: vram, dst_at: 0, src: &block, src_at: 0, len: size }];
            if backend.copy(&ops).is_ok() {
                add(&format!("the card copies {} into card memory (wait included)", kind.name()), size, best(5, size, || {
                    let _ = backend.copy(&ops);
                }));
            }
        }
    }
    if let Some(vram) = &vram {
        if let Ok(other) = backend.create_block(HeapKind::Vram, size) {
            let ops = [CopyOp { dst: &other, dst_at: 0, src: vram, src_at: 0, len: size }];
            add("the card copies card memory into card memory (wait included)", size, best(5, size, || {
                let _ = backend.copy(&ops);
            }));
        }
        add("card memory read back by the CPU through a copy (buffer made, copy, wait, read)", size, best(3, size, || {
            let _ = backend.read_back(vram, 0, &mut sink);
        }));
    }
    out
}
