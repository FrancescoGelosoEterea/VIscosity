//! The Direct3D 12 backend: a small renderer whose frames are recorded on a
//! pool's workers.
//!
//! What is kept from frame to frame instead of made again: the command lists
//! (one a part, reset each frame), three sets of command allocators (a set is
//! used again once the card has run the frame it recorded, so nothing waits
//! for the frame just sent), the pipeline, the target and its view. Every
//! frame leaves two timestamps on the card, around its draws.
//!
//! The same frame can be given as one indirect call instead: the draws'
//! constants and arguments written into a buffer (on the pool) and one
//! `ExecuteIndirect` for all of them, which is what a renderer that leaves
//! the list of draws to the card looks like from the CPU's side.

use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, RECT};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_FEATURE_LEVEL_12_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, ID3DBlob};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::{Interface, PCSTR, s};

use crate::record::{FrameRecord, RecordBackend, RecordError, record_frame};
use crate::sched::WorkerPool;

/// One draw: a triangle at `offset` (in clip space), `scale` of the base
/// triangle's size, in `colour` (red in the low byte).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Draw {
    pub offset: [f32; 2],
    pub scale: f32,
    pub colour: u32,
}

/// A draw's record in the argument buffer: its constants, then the draw.
#[repr(C)]
#[derive(Clone, Copy)]
struct Indirect {
    draw: Draw,
    arguments: D3D12_DRAW_ARGUMENTS,
}

pub const WIDTH: u32 = 1280;
pub const HEIGHT: u32 = 720;
const SETS: usize = 3;

const SHADERS: &str = r#"
#define RS "RootFlags(ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT), RootConstants(num32BitConstants=4, b0)"
cbuffer Draw : register(b0) { float2 offset; float scale; uint colour; };
[RootSignature(RS)]
float4 vs(float2 p : POSITION) : SV_Position { return float4(p * scale + offset, 0.5, 1.0); }
float4 ps() : SV_Target { return float4(float3(colour & 255u, (colour >> 8) & 255u, (colour >> 16) & 255u) / 255.0, 1.0); }
"#;

fn device_error(e: windows::core::Error) -> RecordError {
    RecordError::Device(e.to_string())
}

fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>, RecordError> {
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    // SAFETY: the source and names live through the call; the out pointers are ours.
    let compiled = unsafe { D3DCompile(SHADERS.as_ptr().cast(), SHADERS.len(), s!("frameprep"), None, None, entry, target, 0, 0, &mut code, Some(&mut errors)) };
    // SAFETY: a blob's own bytes.
    let bytes = |b: &ID3DBlob| unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }.to_vec();
    match (compiled, code) {
        (Ok(()), Some(code)) => Ok(bytes(&code)),
        (e, _) => Err(RecordError::Device(format!("shader: {:?} {}", e.err(), errors.map(|b| String::from_utf8_lossy(&bytes(&b)).into_owned()).unwrap_or_default()))),
    }
}

fn transition(resource: &ID3D12Resource, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: std::mem::ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                // SAFETY: a borrowed pointer the barrier never releases.
                pResource: unsafe { std::mem::transmute_copy(resource) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

fn buffer(size: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size.max(1),
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    }
}

/// What changes from frame to frame: which allocators record now, and the
/// lists, one a part.
struct Recording {
    set: usize,
    /// `SETS` sets of one allocator a worker.
    allocators: Vec<Vec<ID3D12CommandAllocator>>,
    lists: Vec<ID3D12GraphicsCommandList>,
}

struct Sent {
    /// The fence value of the last frame each set recorded.
    sets: [u64; SETS],
    value: u64,
    /// Two lists of the renderer's own: before and after the parts.
    edge: [ID3D12GraphicsCommandList; 2],
    /// The argument buffer's records.
    arguments: (ID3D12Resource, *mut Indirect, usize),
}

/// A device, a target and everything a frame of triangles needs.
pub struct Renderer {
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    fence: ID3D12Fence,
    event: HANDLE,
    target: ID3D12Resource,
    _views: ID3D12DescriptorHeap,
    view: D3D12_CPU_DESCRIPTOR_HANDLE,
    root: ID3D12RootSignature,
    pipeline: ID3D12PipelineState,
    vertices: ID3D12Resource,
    signature: ID3D12CommandSignature,
    queries: ID3D12QueryHeap,
    stamps: ID3D12Resource,
    frequency: u64,
    workers: usize,
    name: String,
    recording: RwLock<Recording>,
    sent: Mutex<Sent>,
    /// The parts' closed lists, until the frame's lists go to the queue
    /// together.
    pending: Mutex<Vec<ClosedList>>,
}

// SAFETY: Direct3D 12 objects are free-threaded. A command list and its
// allocator are used by one thread at a time: a part's list by the worker
// that records the part, a worker's allocator by that worker, the rest under
// `sent`'s lock.
unsafe impl Send for Renderer {}
// SAFETY: as above.
unsafe impl Sync for Renderer {}

/// An open list of one part.
pub struct OpenList(ID3D12GraphicsCommandList);
/// A closed one.
pub struct ClosedList(ID3D12CommandList);
// SAFETY: a closed list is only handed to the queue.
unsafe impl Send for ClosedList {}

/// How a frame went.
#[derive(Clone, Debug, Default)]
pub struct FrameStats {
    pub draws: usize,
    /// Getting a set of allocators back (a wait if the card still runs the
    /// frame that used it) and resetting it.
    pub prepare: Duration,
    pub record: FrameRecord,
    /// The card's time between the frame's two timestamps.
    pub gpu: Duration,
    /// From the first list handed over until the card had run the last.
    pub latency: Duration,
}

impl Renderer {
    /// A renderer on the fastest adapter that is not software, or on the
    /// software one (`software`), for pools of up to `workers` workers.
    pub fn new(workers: usize, software: bool) -> Result<Renderer, RecordError> {
        let workers = workers.max(1);
        // SAFETY: plain object creation, each result checked.
        unsafe {
            let factory: IDXGIFactory6 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).map_err(device_error)?;
            let mut found = None;
            for i in 0.. {
                let Ok(adapter) = factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(i, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE) else {
                    break;
                };
                let desc = adapter.GetDesc1().map_err(device_error)?;
                if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0) != software {
                    continue;
                }
                let mut device: Option<ID3D12Device> = None;
                if D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_12_0, &mut device).is_ok()
                    && let Some(device) = device
                {
                    let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
                    found = Some((device, String::from_utf16_lossy(&desc.Description[..len])));
                    break;
                }
            }
            let (device, name) = found.ok_or(RecordError::Unsupported("a Direct3D 12 adapter of the kind asked for"))?;
            let queue: ID3D12CommandQueue = device
                .CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })
                .map_err(device_error)?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE).map_err(device_error)?;
            let event = CreateEventW(None, false, false, None).map_err(device_error)?;

            // The target and its view.
            let mut target: Option<ID3D12Resource> = None;
            let texture = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: u64::from(WIDTH),
                Height: HEIGHT,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                ..Default::default()
            };
            let default = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
            let upload = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_UPLOAD, ..Default::default() };
            let readback = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };
            device
                .CreateCommittedResource(&default, D3D12_HEAP_FLAG_NONE, &texture, D3D12_RESOURCE_STATE_RENDER_TARGET, None, &mut target)
                .map_err(device_error)?;
            let target = target.ok_or(RecordError::Device("no target".into()))?;
            let views: ID3D12DescriptorHeap = device
                .CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC { Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV, NumDescriptors: 1, ..Default::default() })
                .map_err(device_error)?;
            let view = views.GetCPUDescriptorHandleForHeapStart();
            device.CreateRenderTargetView(&target, None, view);

            // The pipeline.
            let (vs, ps) = (compile(s!("vs"), s!("vs_5_1"))?, compile(s!("ps"), s!("ps_5_1"))?);
            let root: ID3D12RootSignature = device.CreateRootSignature(0, &vs).map_err(device_error)?;
            let layout = [D3D12_INPUT_ELEMENT_DESC {
                SemanticName: s!("POSITION"),
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                ..Default::default()
            }];
            let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                // A borrowed pointer the descriptor never releases.
                pRootSignature: std::mem::transmute_copy(&root),
                VS: D3D12_SHADER_BYTECODE { pShaderBytecode: vs.as_ptr().cast(), BytecodeLength: vs.len() },
                PS: D3D12_SHADER_BYTECODE { pShaderBytecode: ps.as_ptr().cast(), BytecodeLength: ps.len() },
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC { FillMode: D3D12_FILL_MODE_SOLID, CullMode: D3D12_CULL_MODE_NONE, DepthClipEnable: true.into(), ..Default::default() },
                InputLayout: D3D12_INPUT_LAYOUT_DESC { pInputElementDescs: layout.as_ptr(), NumElements: 1 },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                ..Default::default()
            };
            desc.RTVFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
            desc.BlendState.RenderTarget[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
            let pipeline: ID3D12PipelineState = device.CreateGraphicsPipelineState(&desc).map_err(device_error)?;

            // The base triangle.
            let corners: [[f32; 2]; 3] = [[0.0, 1.0], [1.0, -1.0], [-1.0, -1.0]];
            let mut vertices: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&upload, D3D12_HEAP_FLAG_NONE, &buffer(size_of_val(&corners) as u64), D3D12_RESOURCE_STATE_GENERIC_READ, None, &mut vertices)
                .map_err(device_error)?;
            let vertices = vertices.ok_or(RecordError::Device("no vertex buffer".into()))?;
            let mut data = std::ptr::null_mut();
            vertices.Map(0, None, Some(&mut data)).map_err(device_error)?;
            std::ptr::copy_nonoverlapping(corners.as_ptr().cast::<u8>(), data.cast::<u8>(), size_of_val(&corners));
            vertices.Unmap(0, None);

            // Indirect draws: a record sets the four constants, then draws.
            let arguments = [
                D3D12_INDIRECT_ARGUMENT_DESC {
                    Type: D3D12_INDIRECT_ARGUMENT_TYPE_CONSTANT,
                    Anonymous: D3D12_INDIRECT_ARGUMENT_DESC_0 {
                        Constant: D3D12_INDIRECT_ARGUMENT_DESC_0_1 { RootParameterIndex: 0, DestOffsetIn32BitValues: 0, Num32BitValuesToSet: 4 },
                    },
                },
                D3D12_INDIRECT_ARGUMENT_DESC { Type: D3D12_INDIRECT_ARGUMENT_TYPE_DRAW, ..Default::default() },
            ];
            let mut signature: Option<ID3D12CommandSignature> = None;
            device
                .CreateCommandSignature(
                    &D3D12_COMMAND_SIGNATURE_DESC { ByteStride: size_of::<Indirect>() as u32, NumArgumentDescs: 2, pArgumentDescs: arguments.as_ptr(), NodeMask: 0 },
                    &root,
                    &mut signature,
                )
                .map_err(device_error)?;
            let signature = signature.ok_or(RecordError::Device("no command signature".into()))?;

            // Timestamps: two a set, read back.
            let mut queries: Option<ID3D12QueryHeap> = None;
            device
                .CreateQueryHeap(&D3D12_QUERY_HEAP_DESC { Type: D3D12_QUERY_HEAP_TYPE_TIMESTAMP, Count: 2 * SETS as u32, NodeMask: 0 }, &mut queries)
                .map_err(device_error)?;
            let queries = queries.ok_or(RecordError::Device("no query heap".into()))?;
            let mut stamps: Option<ID3D12Resource> = None;
            device
                .CreateCommittedResource(&readback, D3D12_HEAP_FLAG_NONE, &buffer(16 * SETS as u64), D3D12_RESOURCE_STATE_COPY_DEST, None, &mut stamps)
                .map_err(device_error)?;
            let stamps = stamps.ok_or(RecordError::Device("no timestamp buffer".into()))?;
            let frequency = queue.GetTimestampFrequency().map_err(device_error)?.max(1);

            // Allocators, a worker each, in sets; the renderer's own lists.
            let mut allocators = Vec::new();
            for _ in 0..SETS {
                let mut set = Vec::new();
                for _ in 0..workers {
                    set.push(device.CreateCommandAllocator::<ID3D12CommandAllocator>(D3D12_COMMAND_LIST_TYPE_DIRECT).map_err(device_error)?);
                }
                allocators.push(set);
            }
            let closed = |allocator: &ID3D12CommandAllocator| -> Result<ID3D12GraphicsCommandList, RecordError> {
                let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, allocator, None).map_err(device_error)?;
                list.Close().map_err(device_error)?;
                Ok(list)
            };
            let edge = [closed(&allocators[0][0])?, closed(&allocators[0][0])?];
            let arguments = Self::argument_buffer(&device, 1024)?;
            Ok(Renderer {
                device,
                queue,
                fence,
                event,
                target,
                _views: views,
                view,
                root,
                pipeline,
                vertices,
                signature,
                queries,
                stamps,
                frequency,
                workers,
                name,
                recording: RwLock::new(Recording { set: 0, allocators, lists: Vec::new() }),
                sent: Mutex::new(Sent { sets: [0; SETS], value: 0, edge, arguments }),
                pending: Mutex::new(Vec::new()),
            })
        }
    }

    pub fn adapter_name(&self) -> &str {
        &self.name
    }

    /// The most workers a pool recording with it may have.
    pub fn workers(&self) -> usize {
        self.workers
    }

    fn argument_buffer(device: &ID3D12Device, records: usize) -> Result<(ID3D12Resource, *mut Indirect, usize), RecordError> {
        let upload = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_UPLOAD, ..Default::default() };
        let mut made: Option<ID3D12Resource> = None;
        // SAFETY: plain creation and mapping of an upload buffer.
        unsafe {
            device
                .CreateCommittedResource(&upload, D3D12_HEAP_FLAG_NONE, &buffer((records * size_of::<Indirect>()) as u64), D3D12_RESOURCE_STATE_GENERIC_READ, None, &mut made)
                .map_err(device_error)?;
            let made = made.ok_or(RecordError::Device("no argument buffer".into()))?;
            let mut data = std::ptr::null_mut();
            made.Map(0, None, Some(&mut data)).map_err(device_error)?;
            Ok((made, data.cast(), records))
        }
    }

    fn wait(&self, value: u64) -> Result<(), RecordError> {
        // SAFETY: a live fence and event.
        unsafe {
            while self.fence.GetCompletedValue() < value {
                self.fence.SetEventOnCompletion(value, self.event).map_err(device_error)?;
                WaitForSingleObject(self.event, INFINITE);
            }
            if self.fence.GetCompletedValue() == u64::MAX {
                return Err(RecordError::Device("the device was removed".into()));
            }
        }
        Ok(())
    }

    /// What every list of a frame needs set.
    ///
    /// # Safety
    /// An open list.
    unsafe fn set_state(&self, list: &ID3D12GraphicsCommandList) {
        let viewport = D3D12_VIEWPORT { Width: WIDTH as f32, Height: HEIGHT as f32, MaxDepth: 1.0, ..Default::default() };
        let scissor = RECT { left: 0, top: 0, right: WIDTH as i32, bottom: HEIGHT as i32 };
        let vertices = D3D12_VERTEX_BUFFER_VIEW {
            // SAFETY: a live buffer.
            BufferLocation: unsafe { self.vertices.GetGPUVirtualAddress() },
            SizeInBytes: 24,
            StrideInBytes: 8,
        };
        // SAFETY: as the caller says; live objects.
        unsafe {
            list.SetGraphicsRootSignature(&self.root);
            list.RSSetViewports(&[viewport]);
            list.RSSetScissorRects(&[scissor]);
            list.OMSetRenderTargets(1, Some(&self.view), false, None);
            list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            list.IASetVertexBuffers(0, Some(&[vertices]));
        }
    }

    /// The next set of allocators, reset, once the card has run the frame
    /// that last recorded with it; and a list for each of `parts`.
    fn begin_frame(&self, sent: &Sent, parts: usize) -> Result<usize, RecordError> {
        let mut recording = self.recording.write().unwrap_or_else(|e| e.into_inner());
        let set = (recording.set + 1) % SETS;
        self.wait(sent.sets[set])?;
        // SAFETY: the card has run everything these allocators recorded.
        unsafe {
            for allocator in &recording.allocators[set] {
                allocator.Reset().map_err(device_error)?;
            }
            while recording.lists.len() < parts {
                let list: ID3D12GraphicsCommandList =
                    self.device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &recording.allocators[set][0], None).map_err(device_error)?;
                list.Close().map_err(device_error)?;
                recording.lists.push(list);
            }
        }
        recording.set = set;
        Ok(set)
    }

    /// Records one of the renderer's own two lists and closes it.
    fn edge(&self, sent: &Sent, which: usize, set: usize, record: impl FnOnce(&ID3D12GraphicsCommandList)) -> Result<ID3D12CommandList, RecordError> {
        let recording = self.recording.read().unwrap_or_else(|e| e.into_inner());
        let list = &sent.edge[which];
        // SAFETY: the caller's own allocator of the set, with no list open on it.
        unsafe {
            list.Reset(&recording.allocators[set][0], &self.pipeline).map_err(device_error)?;
            record(list);
            list.Close().map_err(device_error)?;
        }
        list.cast().map_err(device_error)
    }

    /// Hands the frame's lists to the queue in one call (the first with the
    /// opening timestamp, the parts in order, the last with the closing
    /// one, so the two stamps bound the card's work and nothing of the
    /// CPU's), waits for the card, and returns the call's time, the card's
    /// time and the time until the card was done.
    fn finish(&self, sent: &mut Sent, set: usize, first: ID3D12CommandList, parts: Vec<ID3D12CommandList>) -> Result<(Duration, Duration, Duration), RecordError> {
        let last = self.edge(sent, 1, set, |list| {
            // SAFETY: an open list; live heap and buffer.
            unsafe {
                list.EndQuery(&self.queries, D3D12_QUERY_TYPE_TIMESTAMP, 2 * set as u32 + 1);
                list.ResolveQueryData(&self.queries, D3D12_QUERY_TYPE_TIMESTAMP, 2 * set as u32, 2, &self.stamps, 16 * set as u64);
            }
        })?;
        let lists: Vec<Option<ID3D12CommandList>> = std::iter::once(first).chain(parts).chain(std::iter::once(last)).map(Some).collect();
        let started = Instant::now();
        // SAFETY: closed lists of this queue's kind.
        unsafe { self.queue.ExecuteCommandLists(&lists) };
        let submit = started.elapsed();
        sent.value += 1;
        sent.sets[set] = sent.value;
        // SAFETY: a live queue and fence.
        unsafe { self.queue.Signal(&self.fence, sent.value) }.map_err(device_error)?;
        self.wait(sent.value)?;
        let latency = started.elapsed();
        let mut data = std::ptr::null_mut();
        let range = D3D12_RANGE { Begin: 16 * set, End: 16 * set + 16 };
        // SAFETY: the read-back buffer holds the set's two stamps once the card ran the frame.
        let stamps = unsafe {
            self.stamps.Map(0, Some(&range), Some(&mut data)).map_err(device_error)?;
            let stamps = data.cast::<u8>().add(16 * set).cast::<[u64; 2]>().read_unaligned();
            self.stamps.Unmap(0, None);
            stamps
        };
        let gpu = Duration::from_secs_f64(stamps[1].saturating_sub(stamps[0]) as f64 / self.frequency as f64);
        Ok((submit, gpu, latency))
    }

    fn start(&self, sent: &Sent, set: usize) -> Result<ID3D12CommandList, RecordError> {
        self.edge(sent, 0, set, |list| {
            // SAFETY: an open list; live heap and view.
            unsafe {
                list.EndQuery(&self.queries, D3D12_QUERY_TYPE_TIMESTAMP, 2 * set as u32);
                list.ClearRenderTargetView(self.view, &[0.0, 0.0, 0.0, 1.0], None);
            }
        })
    }

    /// One frame: `draws` recorded on `pool` in `parts` lists, run in order,
    /// and waited for. The pool has at most [`Renderer::workers`] workers.
    pub fn frame(&self, pool: &WorkerPool, draws: &[Draw], parts: usize) -> Result<FrameStats, RecordError> {
        assert!(pool.workers() <= self.workers, "the renderer was made for fewer workers");
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        let preparing = Instant::now();
        let set = self.begin_frame(&sent, parts.clamp(1, draws.len().max(1)))?;
        let prepare = preparing.elapsed();
        let first = self.start(&sent, set)?;
        let mut record = record_frame(pool, self, draws, parts)?;
        let parts: Vec<ID3D12CommandList> = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner())).into_iter().map(|p| p.0).collect();
        let (submit, gpu, latency) = self.finish(&mut sent, set, first, parts)?;
        record.submit = submit;
        Ok(FrameStats { draws: draws.len(), prepare, record, gpu, latency })
    }

    /// The same frame as one indirect call: the records written on `pool`,
    /// one list, one `ExecuteIndirect`. `record.run` is the writing.
    pub fn frame_indirect(&self, pool: &WorkerPool, draws: &[Draw]) -> Result<FrameStats, RecordError> {
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        let preparing = Instant::now();
        let set = self.begin_frame(&sent, 1)?;
        if sent.arguments.2 < draws.len() {
            // (The old buffer may still be read by the frame before: that frame is waited for first.)
            self.wait(sent.value)?;
            sent.arguments = Self::argument_buffer(&self.device, draws.len().next_power_of_two())?;
        }
        let prepare = preparing.elapsed();
        let first = self.start(&sent, set)?;
        struct Out(*mut Indirect);
        // SAFETY: the pieces a run hands out are apart.
        unsafe impl Sync for Out {}
        let out = Out(sent.arguments.1);
        let run = pool.parallel_for(draws.len(), 2048, &|piece, _| {
            let out = &out;
            for i in piece {
                let record = Indirect { draw: draws[i], arguments: D3D12_DRAW_ARGUMENTS { VertexCountPerInstance: 3, InstanceCount: 1, StartVertexLocation: 0, StartInstanceLocation: 0 } };
                // SAFETY: record `i` of a mapped buffer of at least `draws.len()` records.
                unsafe { out.0.add(i).write_unaligned(record) };
            }
        });
        let recording = self.recording.read().unwrap_or_else(|e| e.into_inner());
        let list = recording.lists[0].clone();
        // SAFETY: the caller's allocator of the set, with no list open on it; live objects.
        unsafe {
            list.Reset(&recording.allocators[set][0], &self.pipeline).map_err(device_error)?;
            self.set_state(&list);
            if !draws.is_empty() {
                list.ExecuteIndirect(&self.signature, draws.len() as u32, &sent.arguments.0, 0, None::<&ID3D12Resource>, 0);
            }
            list.Close().map_err(device_error)?;
        }
        drop(recording);
        let (submit, gpu, latency) = self.finish(&mut sent, set, first, vec![list.cast().map_err(device_error)?])?;
        Ok(FrameStats { draws: draws.len(), prepare, record: FrameRecord { parts: 1, run, submit }, gpu, latency })
    }

    /// The target's pixels as the last frame left them: rows of
    /// [`WIDTH`] RGBA pixels.
    pub fn read_target(&self) -> Result<Vec<u8>, RecordError> {
        let sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        self.wait(sent.value)?;
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut total = 0u64;
        let readback = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };
        let mut landing: Option<ID3D12Resource> = None;
        // SAFETY: live objects; the copy is waited for before the bytes are read.
        unsafe {
            let desc = self.target.GetDesc();
            self.device.GetCopyableFootprints(&desc, 0, 1, 0, Some(&mut footprint), None, None, Some(&mut total));
            self.device
                .CreateCommittedResource(&readback, D3D12_HEAP_FLAG_NONE, &buffer(total), D3D12_RESOURCE_STATE_COPY_DEST, None, &mut landing)
                .map_err(device_error)?;
            let landing = landing.ok_or(RecordError::Device("no read-back buffer".into()))?;
            let allocator: ID3D12CommandAllocator = self.device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT).map_err(device_error)?;
            let list: ID3D12GraphicsCommandList = self.device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None).map_err(device_error)?;
            let to = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&landing),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: footprint },
            };
            let from = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&self.target),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
            };
            list.ResourceBarrier(&[transition(&self.target, D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
            list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
            list.ResourceBarrier(&[transition(&self.target, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_RENDER_TARGET)]);
            list.Close().map_err(device_error)?;
            self.queue.ExecuteCommandLists(&[Some(list.cast().map_err(device_error)?)]);
            let once: ID3D12Fence = self.device.CreateFence(0, D3D12_FENCE_FLAG_NONE).map_err(device_error)?;
            self.queue.Signal(&once, 1).map_err(device_error)?;
            while once.GetCompletedValue() < 1 {
                once.SetEventOnCompletion(1, self.event).map_err(device_error)?;
                WaitForSingleObject(self.event, INFINITE);
            }
            let mut data = std::ptr::null_mut();
            landing.Map(0, None, Some(&mut data)).map_err(device_error)?;
            let (row, pitch) = (WIDTH as usize * 4, footprint.Footprint.RowPitch as usize);
            let mut pixels = vec![0u8; row * HEIGHT as usize];
            for y in 0..HEIGHT as usize {
                std::ptr::copy_nonoverlapping(data.cast::<u8>().add(footprint.Offset as usize + y * pitch), pixels.as_mut_ptr().add(y * row), row);
            }
            landing.Unmap(0, None);
            Ok(pixels)
        }
    }
}

impl RecordBackend for Renderer {
    type Item = Draw;
    type List = OpenList;
    type Closed = ClosedList;

    fn begin(&self, worker: usize, part: usize) -> Result<OpenList, RecordError> {
        let recording = self.recording.read().unwrap_or_else(|e| e.into_inner());
        let list = recording.lists[part].clone();
        // SAFETY: the part's own list, which no other worker has, and this
        // worker's allocator, on which it has no other list open.
        unsafe {
            list.Reset(&recording.allocators[recording.set][worker], &self.pipeline).map_err(device_error)?;
            self.set_state(&list);
        }
        Ok(OpenList(list))
    }

    fn record(&self, list: &mut OpenList, draw: &Draw) {
        // SAFETY: an open list with the frame's state set; four dwords of a `Draw`.
        unsafe {
            list.0.SetGraphicsRoot32BitConstants(0, 4, std::ptr::from_ref(draw).cast(), 0);
            list.0.DrawInstanced(3, 1, 0, 0);
        }
    }

    fn end(&self, list: OpenList) -> Result<ClosedList, RecordError> {
        // SAFETY: an open list.
        unsafe { list.0.Close() }.map_err(device_error)?;
        Ok(ClosedList(list.0.cast().map_err(device_error)?))
    }

    /// Kept until the frame's own first and last lists go with them
    /// ([`Renderer::finish`]): one call to the queue a frame.
    fn submit(&self, parts: Vec<ClosedList>) -> Result<(), RecordError> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).extend(parts);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Triangles that overlap, each in its own colour: the picture depends
    /// on the order they are drawn in.
    fn scene(count: usize) -> Vec<Draw> {
        (0..count)
            .map(|i| {
                let t = i as f32 / count as f32;
                Draw {
                    offset: [(t * 37.0).sin() * 0.7, (t * 23.0).cos() * 0.7],
                    scale: 0.05 + 0.25 * ((i * 7) % 11) as f32 / 11.0,
                    colour: 0x0040_4040 | (i as u32).wrapping_mul(2_654_435_761) & 0x00FF_FFFF,
                }
            })
            .collect()
    }

    fn pictures_agree(software: bool) {
        let Ok(renderer) = Renderer::new(16, software) else {
            eprintln!("no adapter (software: {software}): skipped");
            return;
        };
        let draws = scene(3000);
        let one = WorkerPool::new(1);
        renderer.frame(&one, &draws, 1).unwrap();
        let reference = renderer.read_target().unwrap();
        assert!(reference.chunks(4).filter(|p| p[..3] != [0, 0, 0]).count() > 50_000, "the frame drew something");
        for (workers, parts) in [(4, 4), (8, 64), (16, 300)] {
            let pool = WorkerPool::new(workers);
            for _ in 0..4 {
                let stats = renderer.frame(&pool, &draws, parts).unwrap();
                assert_eq!((stats.draws, stats.record.parts), (3000, parts));
            }
            assert!(renderer.read_target().unwrap() == reference, "{workers} workers, {parts} parts: another picture than one list gives");
        }
        // And as one indirect call.
        let pool = WorkerPool::new(8);
        renderer.frame_indirect(&pool, &draws).unwrap();
        assert!(renderer.read_target().unwrap() == reference, "the indirect call gives another picture");
        // An empty frame is the cleared target.
        renderer.frame(&pool, &[], 8).unwrap();
        assert!(renderer.read_target().unwrap().chunks(4).all(|p| p == [0, 0, 0, 255]));
    }

    #[test]
    fn many_lists_give_the_picture_one_list_gives_on_the_card() {
        pictures_agree(false);
    }

    #[test]
    fn many_lists_give_the_picture_one_list_gives_on_the_software_adapter() {
        pictures_agree(true);
    }
}
