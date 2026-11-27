//! The Direct3D 12 backend: real heaps of each kind, copies on a queue of its
//! own, residency through the device, budgets from DXGI.
//!
//! | [`HeapKind`] | heap | state | mapped |
//! |---|---|---|---|
//! | `Vram` | `DEFAULT` | common | no |
//! | `MappedVram` | `GPU_UPLOAD` | common | yes |
//! | `SystemRam` | custom, pool L0, CPU pages written back | common | yes |
//! | `Unified` | custom, pool L0, written back or write-combined | common | yes |
//!
//! Blocks are committed buffers. A buffer in the common state takes the state
//! a copy needs by itself and falls back afterwards, so copies need no
//! barriers.
//!
//! System memory is a custom heap in pool L0 with cached CPU pages, not an
//! upload heap: an upload heap's pages are write-combined (the CPU reads them
//! at some 45 MB/s here) and, measured on an RTX 5070 with a resizable BAR,
//! the card copied out of one as fast as out of its own memory, which system
//! memory behind a bus cannot be: where the driver keeps an upload heap is
//! its own business. Pool L0 on a discrete adapter is the PC's memory.

use std::ptr::NonNull;
use std::sync::Mutex;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_12_0;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

use crate::backend::{Backend, BackendError, Budget, CopyOp, HeapKind};
use crate::hw::Adapter;
use crate::policy::Caps;

fn device_error(e: windows::core::Error) -> BackendError {
    if e.code() == windows::Win32::Foundation::E_OUTOFMEMORY || e.code() == DXGI_ERROR_DEVICE_REMOVED {
        return BackendError::OutOfMemory("device memory");
    }
    BackendError::Device(e.to_string())
}

fn budget_of(adapter: &IDXGIAdapter3) -> Budget {
    let mut own = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
    let mut shared = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
    // SAFETY: plain queries that fill our structures.
    unsafe {
        let _ = adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut own);
        let _ = adapter.QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut shared);
    }
    Budget { local_budget: own.Budget, local_used: own.CurrentUsage, shared_budget: shared.Budget, shared_used: shared.CurrentUsage }
}

/// What the device says of its memory: (unified, cache coherent, card memory
/// the CPU maps).
fn architecture(device: &ID3D12Device) -> (bool, bool, bool) {
    let mut arch = D3D12_FEATURE_DATA_ARCHITECTURE1::default();
    let mut options = D3D12_FEATURE_DATA_D3D12_OPTIONS16::default();
    // SAFETY: each call fills a structure of the size it is given.
    unsafe {
        let _ = device.CheckFeatureSupport(D3D12_FEATURE_ARCHITECTURE1, (&raw mut arch).cast(), size_of_val(&arch) as u32);
        let _ = device.CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS16, (&raw mut options).cast(), size_of_val(&options) as u32);
    }
    (arch.UMA.as_bool(), arch.CacheCoherentUMA.as_bool(), options.GPUUploadHeapSupported.as_bool())
}

fn enumerate() -> Vec<IDXGIAdapter1> {
    let mut found = Vec::new();
    // SAFETY: plain enumeration.
    unsafe {
        let Ok(factory) = CreateDXGIFactory2::<IDXGIFactory6>(DXGI_CREATE_FACTORY_FLAGS(0)) else {
            return found;
        };
        for i in 0.. {
            match factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(i, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE) {
                Ok(adapter) => found.push(adapter),
                Err(_) => break,
            }
        }
    }
    found
}

fn describe(index: u32, adapter: &IDXGIAdapter1) -> (Adapter, Option<ID3D12Device>) {
    let mut out = Adapter { index, ..Default::default() };
    // SAFETY: plain queries of a live adapter.
    unsafe {
        if let Ok(desc) = adapter.GetDesc1() {
            let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            out.name = String::from_utf16_lossy(&desc.Description[..len]);
            out.vendor_id = desc.VendorId;
            out.device_id = desc.DeviceId;
            out.software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
            out.dedicated_vram = desc.DedicatedVideoMemory as u64;
            out.dedicated_system = desc.DedicatedSystemMemory as u64;
            out.shared_system = desc.SharedSystemMemory as u64;
        }
        if let Ok(adapter3) = adapter.cast::<IDXGIAdapter3>() {
            out.budget = budget_of(&adapter3);
        }
        let mut device: Option<ID3D12Device> = None;
        if D3D12CreateDevice(adapter, D3D_FEATURE_LEVEL_12_0, &mut device).is_ok()
            && let Some(device) = device
        {
            out.d3d12 = true;
            (out.unified, out.cache_coherent, out.mapped_vram) = architecture(&device);
            return (out, Some(device));
        }
    }
    (out, None)
}

/// Every adapter, described ([`crate::hw::adapters`]).
pub(crate) fn probe() -> Vec<Adapter> {
    enumerate().iter().enumerate().map(|(i, a)| describe(i as u32, a).0).collect()
}

/// Which adapter a backend is made on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// The fastest adapter that is not software.
    Fastest,
    /// By its place in [`crate::hw::adapters`].
    Index(u32),
    /// The system's software rasterizer: a unified adapter on any machine, for
    /// tests of that path (its speed says nothing).
    Software,
}

struct Lane {
    allocator: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    /// Signalled when the fence reaches the value waited for.
    event: HANDLE,
    value: u64,
}

/// A device and what the pool needs of it.
pub struct D3d12 {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    adapter: Option<IDXGIAdapter3>,
    info: Adapter,
    lane: Mutex<Lane>,
}

// SAFETY: Direct3D 12 devices, queues and fences are free-threaded; the
// command list and its allocator are used only under `lane`'s lock.
unsafe impl Send for D3d12 {}
// SAFETY: as above.
unsafe impl Sync for D3d12 {}

/// A committed buffer of one kind.
pub struct Block {
    resource: ID3D12Resource,
    mapped: Option<NonNull<u8>>,
    kind: HeapKind,
    size: u64,
}

// SAFETY: a free-threaded resource and its persistent mapping, which the pool
// reaches only under its lock.
unsafe impl Send for Block {}
// SAFETY: as above.
unsafe impl Sync for Block {}

impl Block {
    pub fn resource(&self) -> &ID3D12Resource {
        &self.resource
    }

    pub fn kind(&self) -> HeapKind {
        self.kind
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

fn buffer_desc(size: u64, flags: D3D12_RESOURCE_FLAGS) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size.max(1),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_UNKNOWN,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        Flags: flags,
        ..Default::default()
    }
}

impl D3d12 {
    pub fn open(which: Which) -> Result<D3d12, BackendError> {
        let all = enumerate();
        let described: Vec<(Adapter, Option<ID3D12Device>)> = all.iter().enumerate().map(|(i, a)| describe(i as u32, a)).collect();
        let pick = described
            .iter()
            .position(|(info, device)| {
                device.is_some()
                    && match which {
                        Which::Fastest => !info.software,
                        Which::Index(i) => info.index == i,
                        Which::Software => info.software,
                    }
            })
            .ok_or(BackendError::Unsupported("a Direct3D 12 adapter of the kind asked for"))?;
        let (info, device) = described.into_iter().nth(pick).expect("found above");
        let device = device.expect("found above");
        // SAFETY: plain object creation on a live device.
        unsafe {
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                .map_err(device_error)?;
            let allocator: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).map_err(device_error)?;
            let list: ID3D12GraphicsCommandList =
                device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).map_err(device_error)?;
            list.Close().map_err(device_error)?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).map_err(device_error)?;
            // (Auto-reset, kept for as long as the process runs.)
            let event = CreateEventW(None, false, false, None).map_err(device_error)?;
            Ok(D3d12 { device, queue, adapter: all[pick].cast().ok(), info, lane: Mutex::new(Lane { allocator, list, fence, event, value: 0 }) })
        }
    }

    /// The adapter the device is on.
    pub fn adapter(&self) -> &Adapter {
        &self.info
    }

    pub fn device(&self) -> &ID3D12Device {
        &self.device
    }

    /// Records with `record`, runs the list and waits until the card is done.
    fn run(&self, record: impl FnOnce(&ID3D12GraphicsCommandList)) -> Result<(), BackendError> {
        let mut lane = self.lane.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the list's last run was waited for below; live objects.
        unsafe {
            lane.allocator.Reset().map_err(device_error)?;
            lane.list.Reset(&lane.allocator, None).map_err(device_error)?;
            record(&lane.list);
            lane.list.Close().map_err(device_error)?;
            self.queue.ExecuteCommandLists(&[Some(lane.list.cast().map_err(device_error)?)]);
            lane.value += 1;
            self.queue.Signal(&lane.fence, lane.value).map_err(device_error)?;
            // Until the card has passed the signal: the caller may free,
            // evict or read what the list touched as soon as this returns.
            while lane.fence.GetCompletedValue() < lane.value {
                lane.fence.SetEventOnCompletion(lane.value, lane.event).map_err(device_error)?;
                WaitForSingleObject(lane.event, INFINITE);
            }
            if lane.fence.GetCompletedValue() == u64::MAX {
                return Err(BackendError::Device("the device was removed".into()));
            }
        }
        Ok(())
    }

    fn committed(&self, heap: &D3D12_HEAP_PROPERTIES, size: u64, state: D3D12_RESOURCE_STATES) -> Result<ID3D12Resource, BackendError> {
        let mut resource: Option<ID3D12Resource> = None;
        // SAFETY: plain creation of a buffer.
        unsafe { self.device.CreateCommittedResource(heap, D3D12_HEAP_FLAG_NONE, &buffer_desc(size, D3D12_RESOURCE_FLAG_NONE), state, None, &mut resource) }
            .map_err(device_error)?;
        resource.ok_or(BackendError::Device("no resource came back".into()))
    }
}

impl Backend for D3d12 {
    type Block = Block;

    fn caps(&self) -> Caps {
        Caps { unified: self.info.unified, mapped_vram: self.info.mapped_vram }
    }

    fn budget(&self) -> Budget {
        self.adapter.as_ref().map(budget_of).unwrap_or_default()
    }

    fn create_block(&self, kind: HeapKind, size: u64) -> Result<Block, BackendError> {
        let of = |t| D3D12_HEAP_PROPERTIES { Type: t, ..Default::default() };
        let (heap, state) = match kind {
            HeapKind::Vram => (of(D3D12_HEAP_TYPE_DEFAULT), D3D12_RESOURCE_STATE_COMMON),
            HeapKind::MappedVram => {
                if !self.info.mapped_vram {
                    return Err(BackendError::Unsupported("card memory the CPU maps (GPU upload heaps)"));
                }
                (of(D3D12_HEAP_TYPE_GPU_UPLOAD), D3D12_RESOURCE_STATE_COMMON)
            }
            HeapKind::SystemRam | HeapKind::Unified => {
                if kind == HeapKind::Unified && !self.info.unified {
                    return Err(BackendError::Unsupported("unified memory (the adapter has memory of its own)"));
                }
                // The PC's memory, pool L0. Cached pages, so the CPU reads it
                // at memory speed; on a unified adapter whose caches do not
                // see the other side's writes at once, write-combined ones.
                let cached = !self.info.unified || self.info.cache_coherent;
                let heap = D3D12_HEAP_PROPERTIES {
                    Type: D3D12_HEAP_TYPE_CUSTOM,
                    CPUPageProperty: if cached { D3D12_CPU_PAGE_PROPERTY_WRITE_BACK } else { D3D12_CPU_PAGE_PROPERTY_WRITE_COMBINE },
                    MemoryPoolPreference: D3D12_MEMORY_POOL_L0,
                    ..Default::default()
                };
                (heap, D3D12_RESOURCE_STATE_COMMON)
            }
        };
        let resource = self.committed(&heap, size, state)?;
        let mapped = if kind.mapped() {
            let mut data = std::ptr::null_mut();
            // SAFETY: a mappable buffer; the mapping lasts as long as it does.
            unsafe { resource.Map(0, None, Some(&mut data)) }.map_err(device_error)?;
            Some(NonNull::new(data.cast::<u8>()).ok_or(BackendError::Device("a mapping at no address".into()))?)
        } else {
            None
        };
        Ok(Block { resource, mapped, kind, size })
    }

    fn mapped(&self, block: &Block) -> Option<NonNull<u8>> {
        block.mapped
    }

    fn gpu_address(&self, block: &Block) -> u64 {
        // SAFETY: a live buffer.
        unsafe { block.resource.GetGPUVirtualAddress() }
    }

    fn copy(&self, ops: &[CopyOp<'_, Block>]) -> Result<(), BackendError> {
        if ops.is_empty() {
            return Ok(());
        }
        for op in ops {
            if op.dst_at + op.len > op.dst.size || op.src_at + op.len > op.src.size {
                return Err(BackendError::Device("a copy outside its blocks".into()));
            }
        }
        self.run(|list| {
            for op in ops {
                // SAFETY: an open list; ranges checked above, in live buffers.
                unsafe { list.CopyBufferRegion(&op.dst.resource, op.dst_at, &op.src.resource, op.src_at, op.len) };
            }
        })
    }

    fn read_back(&self, block: &Block, at: u64, out: &mut [u8]) -> Result<(), BackendError> {
        if at + out.len() as u64 > block.size {
            return Err(BackendError::Device("a read outside its block".into()));
        }
        let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };
        let landing = self.committed(&heap, out.len() as u64, D3D12_RESOURCE_STATE_COPY_DEST)?;
        self.run(|list| {
            // SAFETY: an open list; the range lies in both live buffers.
            unsafe { list.CopyBufferRegion(&landing, 0, &block.resource, at, out.len() as u64) };
        })?;
        let mut data = std::ptr::null_mut();
        // SAFETY: the read-back buffer holds `out.len()` bytes once the copy ran.
        unsafe {
            landing.Map(0, None, Some(&mut data)).map_err(device_error)?;
            std::ptr::copy_nonoverlapping(data.cast::<u8>(), out.as_mut_ptr(), out.len());
            landing.Unmap(0, None);
        }
        Ok(())
    }

    fn evict(&self, block: &Block) -> Result<(), BackendError> {
        let object: ID3D12Pageable = block.resource.cast().map_err(device_error)?;
        // SAFETY: a live resource nothing on the queue uses (copies are waited for).
        unsafe { self.device.Evict(&[Some(object)]) }.map_err(device_error)
    }

    fn make_resident(&self, block: &Block) -> Result<(), BackendError> {
        let object: ID3D12Pageable = block.resource.cast().map_err(device_error)?;
        // SAFETY: a live resource.
        unsafe { self.device.MakeResident(&[Some(object)]) }.map_err(device_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::{Config, UnifiedPool};
    use crate::policy::Usage;

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_mul(29).wrapping_add(seed)).collect()
    }

    fn config() -> Config {
        Config { block_size: 4 << 20, staging_size: 1 << 20, mapped_vram_most: 64 << 20, ..Default::default() }
    }

    /// Every kind the adapter has: bytes written come back, through the real
    /// heaps, copies, read-backs, eviction and migration.
    fn round_trip(which: Which) {
        let Ok(backend) = D3d12::open(which) else {
            eprintln!("no adapter for {which:?}: skipped");
            return;
        };
        let caps = backend.caps();
        eprintln!("{}\n", backend.adapter());
        let pool = UnifiedPool::new(backend, config());
        let mut live = Vec::new();
        for (i, usage) in [Usage::STATIC, Usage::DYNAMIC, Usage::GPU_OUTPUT, Usage::CPU_SIDE].into_iter().enumerate() {
            let len = 1_500_000 + i * 4096;
            let h = pool.allocate(len as u64, 256, usage).unwrap();
            let (kind, why) = pool.placement(h).unwrap();
            eprintln!("{usage:?}\n  -> {}: {why}", kind.name());
            if caps.unified {
                assert_eq!(kind, HeapKind::Unified);
            }
            let data = pattern(len, i as u8);
            pool.write(h, 0, &data).unwrap();
            pool.write(h, 4096, &data[..1000]).unwrap();
            let mut back = vec![0u8; len];
            pool.read(h, 0, &mut back).unwrap();
            assert_eq!(back[..4096], data[..4096], "{kind:?}");
            assert_eq!(back[4096..5096], data[..1000], "{kind:?}");
            assert_eq!(back[5096..], data[5096..], "{kind:?}");
            assert_ne!(pool.gpu_address(h).unwrap(), 0);
            live.push((h, back));
        }
        // Moved through every other kind the adapter has: the bytes go along.
        for (h, bytes) in &live {
            let kinds: Vec<HeapKind> = if caps.unified {
                vec![]
            } else if caps.mapped_vram {
                vec![HeapKind::SystemRam, HeapKind::MappedVram, HeapKind::Vram]
            } else {
                vec![HeapKind::SystemRam, HeapKind::Vram]
            };
            for kind in kinds {
                pool.migrate(*h, kind).unwrap();
                let mut back = vec![0u8; bytes.len()];
                pool.read(*h, 0, &mut back).unwrap();
                assert_eq!(&back, bytes, "after moving to {kind:?}");
            }
        }
        for _ in 0..3 {
            pool.end_frame().unwrap();
        }
        eprintln!("{}", pool.snapshot());
        for (h, _) in live {
            pool.free(h).unwrap();
        }
    }

    #[test]
    fn the_software_adapter_is_unified_and_round_trips() {
        round_trip(Which::Software);
    }

    #[test]
    fn the_fastest_adapter_round_trips() {
        round_trip(Which::Fastest);
    }

    #[test]
    fn blocks_survive_eviction() {
        let Ok(backend) = D3d12::open(Which::Fastest).or_else(|_| D3d12::open(Which::Software)) else {
            return;
        };
        let kind = if backend.caps().unified { HeapKind::Unified } else { HeapKind::Vram };
        let block = backend.create_block(kind, 1 << 20).unwrap();
        let staging = backend.create_block(HeapKind::SystemRam, 1 << 20).unwrap();
        let data = pattern(1 << 20, 5);
        // SAFETY: the staging block's own mapping, a megabyte long.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), backend.mapped(&staging).unwrap().as_ptr(), data.len()) };
        backend.copy(&[CopyOp { dst: &block, dst_at: 0, src: &staging, src_at: 0, len: 1 << 20 }]).unwrap();
        backend.evict(&block).unwrap();
        backend.make_resident(&block).unwrap();
        let mut back = vec![0u8; 1 << 20];
        backend.read_back(&block, 0, &mut back).unwrap();
        assert_eq!(back, data);
    }
}
