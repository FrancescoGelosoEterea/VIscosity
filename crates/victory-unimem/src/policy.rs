//! Where an allocation goes, from how it is used and what the adapter has.
//!
//! The rules are about the bus between the PC's memory and a discrete card's:
//! bytes cross it at a fraction of the speed either memory has by itself
//! (`crate::bench` measures it on the machine at hand), so an allocation goes
//! where its busiest user is, and data that both sides use is put where the
//! copy can be left out, not where a copy is merely hidden.

use crate::backend::HeapKind;

/// How often one side touches an allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rate {
    Never,
    /// Once, or now and then (a level's geometry, a streamed texture).
    Rare,
    /// Every frame or more.
    PerFrame,
}

/// How an allocation is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub cpu_writes: Rate,
    pub cpu_reads: Rate,
    pub gpu_reads: Rate,
    /// Shaders or copies on the card write it.
    pub gpu_writes: bool,
}

impl Usage {
    /// Filled once by the CPU, read by the card every frame (static geometry,
    /// textures).
    pub const STATIC: Usage = Usage { cpu_writes: Rate::Rare, cpu_reads: Rate::Never, gpu_reads: Rate::PerFrame, gpu_writes: false };
    /// Rewritten by the CPU every frame, read by the card (constants, skinned
    /// vertices, draw arguments).
    pub const DYNAMIC: Usage = Usage { cpu_writes: Rate::PerFrame, cpu_reads: Rate::Never, gpu_reads: Rate::PerFrame, gpu_writes: false };
    /// Written by the card, read back by the CPU now and then.
    pub const GPU_OUTPUT: Usage = Usage { cpu_writes: Rate::Never, cpu_reads: Rate::Rare, gpu_reads: Rate::PerFrame, gpu_writes: true };
    /// The CPU's own working data that the card looks at rarely.
    pub const CPU_SIDE: Usage = Usage { cpu_writes: Rate::PerFrame, cpu_reads: Rate::PerFrame, gpu_reads: Rate::Rare, gpu_writes: false };
}

/// What the adapter has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// The adapter's memory is the PC's (an integrated adapter): one memory,
    /// used in place by both sides.
    pub unified: bool,
    /// The card's memory can be mapped for the CPU.
    pub mapped_vram: bool,
}

/// Room left, in bytes, as the pool sees it when it decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    /// Under the budget of the adapter's own memory.
    pub vram: u64,
    /// Of that, what the pool still lets mapped card memory take.
    pub mapped_vram: u64,
}

/// A placement and why.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    pub kind: HeapKind,
    /// CPU writes go through a staging copy in system memory that the card
    /// then copies: one crossing of the bus per write, counted by the pool.
    pub staged: bool,
    pub why: &'static str,
}

/// Where `size` bytes used as `usage` go.
pub fn place(usage: Usage, size: u64, caps: Caps, room: Room) -> Decision {
    let decide = |kind, staged, why| Decision { kind, staged, why };
    if caps.unified {
        // One memory: nothing to cross, whoever writes.
        return decide(HeapKind::Unified, false, "the adapter shares the PC's memory: used in place by both sides");
    }
    let card_reads = usage.gpu_reads > Rate::Never || usage.gpu_writes;
    if !card_reads {
        return decide(HeapKind::SystemRam, false, "only the CPU uses it");
    }
    if usage.gpu_writes {
        // Shaders cannot write a mapped heap's buffers as unordered access on
        // every driver, and the card's writes belong in its own memory.
        return if size <= room.vram {
            decide(HeapKind::Vram, usage.cpu_writes > Rate::Never, "the card writes it: its own memory; the CPU reads it back by a copy")
        } else {
            decide(HeapKind::SystemRam, false, "the card writes it but its memory is full: system memory, across the bus")
        };
    }
    if usage.cpu_reads == Rate::PerFrame && usage.gpu_reads < Rate::PerFrame {
        return decide(HeapKind::SystemRam, false, "the CPU reads it every frame and the card rarely: it stays with the CPU");
    }
    if usage.cpu_writes == Rate::PerFrame && usage.cpu_reads == Rate::Never && caps.mapped_vram && size <= room.mapped_vram {
        return decide(HeapKind::MappedVram, false, "rewritten every frame and never read by the CPU: written straight into card memory, no copy");
    }
    if size > room.vram {
        return decide(HeapKind::SystemRam, false, "the card's memory is full: system memory, which the card reads across the bus");
    }
    if usage.cpu_writes == Rate::PerFrame && usage.gpu_reads < Rate::PerFrame {
        return decide(HeapKind::SystemRam, false, "rewritten every frame and read rarely by the card: not worth a copy a frame");
    }
    decide(
        HeapKind::Vram,
        usage.cpu_writes > Rate::Never,
        if usage.cpu_writes == Rate::PerFrame {
            "read by the card every frame: its own memory, one staged copy per write"
        } else {
            "written rarely, read by the card every frame: its own memory, copied up once"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISCRETE: Caps = Caps { unified: false, mapped_vram: true };
    const NO_BAR: Caps = Caps { unified: false, mapped_vram: false };
    const ROOM: Room = Room { vram: 4 << 30, mapped_vram: 1 << 30 };
    const FULL: Room = Room { vram: 0, mapped_vram: 0 };

    #[test]
    fn a_unified_adapter_uses_one_memory_for_everything() {
        let caps = Caps { unified: true, mapped_vram: false };
        for usage in [Usage::STATIC, Usage::DYNAMIC, Usage::GPU_OUTPUT, Usage::CPU_SIDE] {
            let d = place(usage, 1 << 20, caps, FULL);
            assert_eq!((d.kind, d.staged), (HeapKind::Unified, false));
        }
    }

    #[test]
    fn a_discrete_card_gets_what_it_reads_every_frame() {
        assert_eq!(place(Usage::STATIC, 1 << 20, DISCRETE, ROOM).kind, HeapKind::Vram);
        assert!(place(Usage::STATIC, 1 << 20, DISCRETE, ROOM).staged);
        // Rewritten every frame: straight into mapped card memory if there is one.
        let d = place(Usage::DYNAMIC, 1 << 20, DISCRETE, ROOM);
        assert_eq!((d.kind, d.staged), (HeapKind::MappedVram, false));
        // Without a mappable card: its own memory, a staged copy per write.
        let d = place(Usage::DYNAMIC, 1 << 20, NO_BAR, ROOM);
        assert_eq!((d.kind, d.staged), (HeapKind::Vram, true));
        // What the card writes is in its own memory.
        assert_eq!(place(Usage::GPU_OUTPUT, 1 << 20, DISCRETE, ROOM).kind, HeapKind::Vram);
        // What the CPU works on stays with the CPU.
        assert_eq!(place(Usage::CPU_SIDE, 1 << 20, DISCRETE, ROOM).kind, HeapKind::SystemRam);
    }

    #[test]
    fn a_full_card_spills_into_system_memory_and_says_so() {
        for usage in [Usage::STATIC, Usage::DYNAMIC, Usage::GPU_OUTPUT] {
            let d = place(usage, 1 << 20, DISCRETE, FULL);
            assert_eq!(d.kind, HeapKind::SystemRam, "{usage:?}");
            assert!(d.why.contains("bus"));
        }
        // Mapped card memory has its own share: full of it, the rest still fits.
        let d = place(Usage::DYNAMIC, 1 << 20, DISCRETE, Room { vram: 1 << 30, mapped_vram: 0 });
        assert_eq!((d.kind, d.staged), (HeapKind::Vram, true));
    }
}
