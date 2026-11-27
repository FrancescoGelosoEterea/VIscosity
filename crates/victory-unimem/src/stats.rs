//! What the pool holds and what it moved, for a monitor or a log line.

use crate::backend::{Budget, HeapKind};

/// One kind of memory, as the pool holds it now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KindStats {
    pub blocks: u64,
    /// Bytes of the blocks, and of the allocations in them.
    pub reserved: u64,
    pub used: u64,
    pub allocations: u64,
    /// Blocks whose memory the system may take now.
    pub evicted_blocks: u64,
}

/// Bytes moved since the pool was made.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traffic {
    /// Written by the CPU straight into where the card reads them: no copy.
    pub direct_bytes: u64,
    /// Written into staging and copied by the card: one crossing each.
    pub staged_bytes: u64,
    /// Copies the card ran for staged writes.
    pub staged_copies: u64,
    /// Read by the CPU from system or unified memory.
    pub read_bytes: u64,
    /// Read by the CPU from mapped card memory (slow) and through read-back
    /// copies of card memory.
    pub slow_read_bytes: u64,
    pub readback_bytes: u64,
    /// Moved when an allocation changed its kind of memory.
    pub migrated_bytes: u64,
    pub evictions: u64,
    pub made_resident: u64,
}

/// The pool at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub frame: u64,
    pub kinds: [KindStats; 4],
    pub traffic: Traffic,
    pub budget: Budget,
}

impl Snapshot {
    pub fn kind(&self, kind: HeapKind) -> &KindStats {
        &self.kinds[kind.index()]
    }

    /// Of the bytes the CPU wrote for the card, the share that needed no copy.
    pub fn direct_share(&self) -> f64 {
        let all = self.traffic.direct_bytes + self.traffic.staged_bytes;
        if all == 0 { 0.0 } else { self.traffic.direct_bytes as f64 / all as f64 }
    }
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 20) as f64
}

impl std::fmt::Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "frame {}", self.frame)?;
        for kind in HeapKind::ALL {
            let k = self.kind(kind);
            if k.blocks == 0 {
                continue;
            }
            writeln!(
                f,
                "  {:<26} {:>4} blocks {:>9.1} MB reserved {:>9.1} MB in {} allocations, {} blocks evicted",
                kind.name(),
                k.blocks,
                mb(k.reserved),
                mb(k.used),
                k.allocations,
                k.evicted_blocks
            )?;
        }
        let t = &self.traffic;
        writeln!(
            f,
            "  written without a copy {:.1} MB, through {} staged copies {:.1} MB ({:.0}% direct); migrated {:.1} MB",
            mb(t.direct_bytes),
            t.staged_copies,
            mb(t.staged_bytes),
            self.direct_share() * 100.0,
            mb(t.migrated_bytes)
        )?;
        writeln!(
            f,
            "  read by the CPU {:.1} MB in place, {:.1} MB from mapped card memory (slow), {:.1} MB by read-back copies",
            mb(t.read_bytes),
            mb(t.slow_read_bytes),
            mb(t.readback_bytes)
        )?;
        write!(
            f,
            "  the system's budget: card {:.0} of {:.0} MB in use, shared {:.0} of {:.0} MB; {} evictions, {} made resident again",
            mb(self.budget.local_used),
            mb(self.budget.local_budget),
            mb(self.budget.shared_used),
            mb(self.budget.shared_budget),
            t.evictions,
            t.made_resident
        )
    }
}
