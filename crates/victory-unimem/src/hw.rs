//! What the machine has: its memory, its adapters, and for each adapter the
//! kinds of memory it offers.
//!
//! Everything here is asked of the system (DXGI, Direct3D 12, the kernel's
//! memory status); nothing is assumed from an adapter's name.
//!
//! About the resizable BAR: Windows does not say how large an adapter's BAR
//! is. What an application can find out, and all it can use, is whether
//! Direct3D 12 offers GPU upload heaps on the adapter (card memory the CPU
//! maps), which needs a resizable BAR and a driver that exposes it.
//! [`Adapter::mapped_vram`] is that answer.

use crate::backend::Budget;

/// The PC's memory, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostMemory {
    pub total_ram: u64,
    pub available_ram: u64,
    /// RAM and page file together: what all processes may commit, and what is
    /// left of it.
    pub commit_limit: u64,
    pub commit_available: u64,
}

/// One adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Adapter {
    /// Its place in the system's order, the fastest first.
    pub index: u32,
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    /// A software rasterizer (the system's own: no card behind it).
    pub software: bool,
    /// Memory of its own; system memory set aside for it; system memory it
    /// may share. As the adapter describes itself.
    pub dedicated_vram: u64,
    pub dedicated_system: u64,
    pub shared_system: u64,
    /// Whether a Direct3D 12 device (feature level 12.0) could be made.
    pub d3d12: bool,
    /// Its memory is the PC's (an integrated adapter), and whether the CPU's
    /// caches see the graphics processor's writes at once.
    pub unified: bool,
    pub cache_coherent: bool,
    /// Card memory can be mapped for the CPU (GPU upload heaps).
    pub mapped_vram: bool,
    /// What the system lets this process use of it now.
    pub budget: Budget,
}

impl Adapter {
    /// A card with memory of its own.
    pub fn discrete(&self) -> bool {
        self.d3d12 && !self.unified && !self.software
    }

    pub fn vendor(&self) -> &'static str {
        match self.vendor_id {
            0x10de => "NVIDIA",
            0x1002 | 0x1022 => "AMD",
            0x8086 => "Intel",
            0x1414 => "Microsoft",
            0x5143 => "Qualcomm",
            _ => "unknown vendor",
        }
    }
}

/// The PC's memory now.
#[cfg(windows)]
pub fn host_memory() -> Option<HostMemory> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: fills a structure of the length it is told.
    unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
    Some(HostMemory {
        total_ram: status.ullTotalPhys,
        available_ram: status.ullAvailPhys,
        commit_limit: status.ullTotalPageFile,
        commit_available: status.ullAvailPageFile,
    })
}

#[cfg(not(windows))]
pub fn host_memory() -> Option<HostMemory> {
    None
}

/// Every adapter of the system, the fastest first.
#[cfg(windows)]
pub fn adapters() -> Vec<Adapter> {
    crate::d3d12::probe()
}

#[cfg(not(windows))]
pub fn adapters() -> Vec<Adapter> {
    Vec::new()
}

fn gb(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

impl std::fmt::Display for HostMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "system memory: {:.1} GB RAM ({:.1} free), {:.1} GB may be committed ({:.1} left)",
            gb(self.total_ram),
            gb(self.available_ram),
            gb(self.commit_limit),
            gb(self.commit_available)
        )
    }
}

impl std::fmt::Display for Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "adapter {}: {} ({}, {:04x}:{:04x}){}", self.index, self.name, self.vendor(), self.vendor_id, self.device_id, if self.software { ", software" } else { "" })?;
        writeln!(
            f,
            "  memory of its own {:.1} GB, system memory set aside {:.1} GB, shared {:.1} GB",
            gb(self.dedicated_vram),
            gb(self.dedicated_system),
            gb(self.shared_system)
        )?;
        if !self.d3d12 {
            return write!(f, "  no Direct3D 12 device (feature level 12.0)");
        }
        writeln!(
            f,
            "  architecture: {}",
            match (self.unified, self.cache_coherent) {
                (true, true) => "unified memory, cache coherent: both sides use one memory in place",
                (true, false) => "unified memory, not cache coherent: one memory, the CPU's view of it uncached",
                (false, _) => "discrete: memory of its own behind a bus",
            }
        )?;
        writeln!(
            f,
            "  card memory the CPU maps (GPU upload heaps, needs a resizable BAR): {}",
            if self.mapped_vram { "yes" } else { "no" }
        )?;
        write!(
            f,
            "  budget now: own {:.1} of {:.1} GB in use, shared {:.1} of {:.1} GB",
            gb(self.budget.local_used),
            gb(self.budget.local_budget),
            gb(self.budget.shared_used),
            gb(self.budget.shared_budget)
        )
    }
}
