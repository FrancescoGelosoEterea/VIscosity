//! The scene's depth and motion vectors for frame generation, taken where
//! the title's own temporal pass reads them.
//!
//! The title resolves its picture over time in `TAA_Utilities`
//! (`CS_ReconstructSurfaces`, then one of eight `CS_SceneColorTemporalResolve*`
//! programs), whichever upscaler follows. Both programs read the scene's
//! depth target (1280x720 at a 720p render: 32-bit float depth with a stencil
//! plane, the frame's one depth target of that size) and its motion vectors
//! (16-bit float pairs in render pixels, the frame's one texture of that
//! format), as a frame capture of 2026-11-27 lists them (list 265: the depth
//! at the reconstruction's first view, the vectors at its second).
//!
//! Right after such a dispatch both are copied into textures of ours (two
//! sets, in turn: a present reads one while the next frame's lists may
//! already be recorded), in the title's list, with views of our own: the
//! title's textures have just been read by its program, so they are in a
//! state a compute shader reads them in, and ours stay in the unordered
//! access state. Frame generation is given ours: 32-bit float depth and the
//! vectors as they are, in a state and a format that never change, whatever
//! the title does with its own textures before the present.

use std::sync::Mutex;

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::s;

use super::{Tagged, once};
use crate::host::HostList;

/// Whether a compute program of the title is one whose dispatch is followed
/// for the depth and motion vectors, by its name in the container.
pub(crate) fn role(container: &[u8]) -> bool {
    let has = |name: &[u8]| container.windows(name.len()).any(|w| w == name);
    has(b"CS_ReconstructSurfaces") || has(b"CS_SceneColorTemporalResolve")
}

/// A texture a followed program reads, as far as the choice needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub format: i32,
    pub flags: i32,
    pub size: [u32; 2],
    /// A plain 2D texture: one slice, one sample.
    pub plain: bool,
}

impl Seen {
    pub(crate) fn of(texture: &ID3D12Resource) -> Seen {
        // SAFETY: a live resource.
        let d = unsafe { texture.GetDesc() };
        Seen {
            format: d.Format.0,
            flags: d.Flags.0,
            size: [d.Width as u32, d.Height],
            plain: d.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D && d.DepthOrArraySize == 1 && d.SampleDesc.Count == 1,
        }
    }
}

/// The format a depth texture's depth is read in by a shader; `None` for a
/// format that holds no 32-bit float depth.
fn depth_view(format: i32) -> Option<DXGI_FORMAT> {
    match format {
        // R32G8X24 typeless, D32 float S8X24, R32 float X8X24.
        19..=21 => Some(DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS),
        // R32 typeless, D32 float, R32 float.
        39..=41 => Some(DXGI_FORMAT_R32_FLOAT),
        _ => None,
    }
}

/// Of the textures a followed program reads, which is the scene's depth and
/// which its motion vectors: the vectors are the one 16-bit float pair
/// texture, the depth a depth target of the same size with 32-bit float
/// depth. `None` if either is missing.
pub(crate) fn pick(seen: &[Seen]) -> Option<(usize, usize)> {
    let motion = seen.iter().position(|s| s.plain && matches!(s.format, 33 | 34) && s.size[0] >= 320 && s.size[1] >= 180)?;
    let depth = seen
        .iter()
        .position(|s| s.plain && s.flags & D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL.0 != 0 && depth_view(s.format).is_some() && s.size == seen[motion].size)?;
    Some((depth, motion))
}

const SOURCE: &str = r#"
#define RS "RootConstants(num32BitConstants=2, b0), DescriptorTable(SRV(t0, numDescriptors=2), UAV(u0, numDescriptors=2))"
cbuffer Size : register(b0) { uint w; uint h; };
Texture2D<float> depth : register(t0);
Texture2D<float2> motion : register(t1);
RWTexture2D<float> depth_out : register(u0);
RWTexture2D<float2> motion_out : register(u1);
[RootSignature(RS)]
[numthreads(8, 8, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    if (id.x >= w || id.y >= h) return;
    depth_out[id.xy] = depth.Load(int3(id.xy, 0));
    motion_out[id.xy] = motion.Load(int3(id.xy, 0));
}
"#;

/// Lists' worth of descriptors in the ring (four each): a list recorded
/// earlier and not run yet must not find its views rewritten.
const SLOTS: u32 = 32;
const UNORDERED: u32 = 0x8;

struct Pass {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    views: ID3D12DescriptorHeap,
    cpu: usize,
    gpu: u64,
    step: u32,
    next: u32,
    /// The two sets of our textures (depth, motion vectors), by size.
    sets: Option<([u32; 2], [[ID3D12Resource; 2]; 2])>,
}
// SAFETY: D3D12 objects are free-threaded; the pass is used under a lock.
unsafe impl Send for Pass {}

/// The pass: not tried yet, could not be made, or made.
static PASS: Mutex<Option<Option<Pass>>> = Mutex::new(None);

fn make(device: &ID3D12Device) -> windows::core::Result<Pass> {
    let code = crate::host::compile(SOURCE, s!("main"), s!("cs_5_1"))?;
    // SAFETY: the root signature is inside the shader; plain creations.
    unsafe {
        let root: ID3D12RootSignature = device.CreateRootSignature(0, &code)?;
        let pso: ID3D12PipelineState = device.CreateComputePipelineState(&D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: std::mem::transmute_copy(&root),
            CS: D3D12_SHADER_BYTECODE { pShaderBytecode: code.as_ptr().cast(), BytecodeLength: code.len() },
            ..Default::default()
        })?;
        let kind = D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV;
        let views: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
            Type: kind,
            NumDescriptors: SLOTS * 4,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            NodeMask: 0,
        })?;
        Ok(Pass {
            cpu: views.GetCPUDescriptorHandleForHeapStart().ptr,
            gpu: views.GetGPUDescriptorHandleForHeapStart().ptr,
            step: device.GetDescriptorHandleIncrementSize(kind),
            root,
            pso,
            views,
            next: 0,
            sets: None,
        })
    }
}

/// Lets the textures of ours go (the check's end, before Streamline shuts
/// down).
pub(crate) fn let_go() {
    if let Some(Some(pass)) = PASS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        pass.sets = None;
    }
}

/// After a followed program's dispatch in `list`: of the textures it reads
/// (`read`, title resources' host copies), the scene's depth and motion
/// vectors are copied into ours. Replaces the list's view heaps, compute root
/// signature and pipeline; false if it recorded nothing.
pub(crate) fn after(list: &HostList, read: &[&ID3D12Resource]) -> bool {
    // (One copy a list: the reconstruction and the resolve share theirs.)
    if list.framegen() != 0 {
        return false;
    }
    let seen: Vec<Seen> = read.iter().map(|t| Seen::of(t)).collect();
    let Some((depth, motion)) = pick(&seen) else {
        once!(warn, read = ?seen, "DLSS Frame Generation: the title's temporal pass names no depth target and motion vectors of one size; no frames are generated");
        return false;
    };
    once!(info, depth = ?seen[depth], motion = ?seen[motion], "DLSS Frame Generation: the scene's depth and motion vectors are copied after the title's temporal pass (first time)");
    record(list, read[depth], read[motion]).is_some()
}

/// Records the copy of `depth` and `motion` (both in a state a compute
/// shader reads them in) into ours, in `list`, and marks the list with the
/// copy's number, which is returned.
pub(crate) fn record(list: &HostList, depth: &ID3D12Resource, motion: &ID3D12Resource) -> Option<u64> {
    let host = crate::host::host()?;
    let device = &host.device;
    let (of_depth, of_motion) = (Seen::of(depth), Seen::of(motion));
    let depth_format = depth_view(of_depth.format)?;
    let size = of_motion.size;
    let mut kept = PASS.lock().unwrap_or_else(|e| e.into_inner());
    if kept.is_none() {
        *kept = Some(make(device).inspect_err(|e| tracing::warn!(error = %e, "DLSS Frame Generation: the pass that copies depth and motion vectors could not be made")).ok());
    }
    let pass = kept.as_mut()?.as_mut()?;
    if pass.sets.as_ref().is_none_or(|s| s.0 != size) {
        let flags = D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
        let set = || -> windows::core::Result<[ID3D12Resource; 2]> {
            Ok([
                super::texture(device, size, DXGI_FORMAT_R32_FLOAT, flags, UNORDERED)?,
                super::texture(device, size, DXGI_FORMAT_R16G16_FLOAT, flags, UNORDERED)?,
            ])
        };
        match (set(), set()) {
            (Ok(a), Ok(b)) => {
                tracing::info!(?size, "DLSS Frame Generation: textures of ours for the scene's depth and motion vectors");
                pass.sets = Some((size, [a, b]));
            }
            (Err(e), _) | (_, Err(e)) => {
                once!(warn, error = %e, "DLSS Frame Generation: no textures for the scene's depth and motion vectors");
                return None;
            }
        }
    }
    let id = super::next_id();
    let [depth_out, motion_out] = pass.sets.as_ref()?.1[(id % 2) as usize].clone();
    let first = (pass.next % SLOTS) * 4;
    pass.next = pass.next.wrapping_add(1);
    let cpu = |i: u32| D3D12_CPU_DESCRIPTOR_HANDLE { ptr: pass.cpu + ((first + i) * pass.step) as usize };
    let gpu = D3D12_GPU_DESCRIPTOR_HANDLE { ptr: pass.gpu + u64::from(first * pass.step) };
    let srv = |format| D3D12_SHADER_RESOURCE_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
        Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
        Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 { Texture2D: D3D12_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1, ..Default::default() } },
    };
    let uav = |format| D3D12_UNORDERED_ACCESS_VIEW_DESC {
        Format: format,
        ViewDimension: D3D12_UAV_DIMENSION_TEXTURE2D,
        Anonymous: D3D12_UNORDERED_ACCESS_VIEW_DESC_0 { Texture2D: D3D12_TEX2D_UAV { MipSlice: 0, PlaneSlice: 0 } },
    };
    let words = [size[0], size[1]];
    let l = &list.list;
    // SAFETY: an open list; slots of our heap; live resources (the title's
    // are held by the list that just read them, ours by `keep`); the pass's
    // objects live as long as the process.
    unsafe {
        device.CreateShaderResourceView(depth, Some(&srv(depth_format)), cpu(0));
        device.CreateShaderResourceView(motion, Some(&srv(DXGI_FORMAT_R16G16_FLOAT)), cpu(1));
        device.CreateUnorderedAccessView(&depth_out, None::<&ID3D12Resource>, Some(&uav(DXGI_FORMAT_R32_FLOAT)), cpu(2));
        device.CreateUnorderedAccessView(&motion_out, None::<&ID3D12Resource>, Some(&uav(DXGI_FORMAT_R16G16_FLOAT)), cpu(3));
        l.SetDescriptorHeaps(&[Some(pass.views.clone())]);
        l.SetComputeRootSignature(&pass.root);
        l.SetPipelineState(&pass.pso);
        l.SetComputeRoot32BitConstants(0, 2, words.as_ptr().cast(), 0);
        l.SetComputeRootDescriptorTable(1, gpu);
        l.Dispatch(size[0].div_ceil(8), size[1].div_ceil(8), 1);
    }
    list.keep(&pass.views);
    list.keep(&depth_out);
    list.keep(&motion_out);
    list.keep(depth);
    list.keep(motion);
    super::noted(id, Tagged(depth_out, UNORDERED), Tagged(motion_out, UNORDERED), size);
    list.mark_framegen(id);
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPTH: Seen = Seen { format: 19, flags: 0x2, size: [1280, 720], plain: true };
    const MOTION: Seen = Seen { format: 34, flags: 0x5, size: [1280, 720], plain: true };

    #[test]
    fn the_programs_followed_by_name() {
        assert!(role(b"..TAA_Utilities:CS_ReconstructSurfaces_94a9c826_Wrapped.."));
        assert!(role(b"..TAA_Utilities:CS_SceneColorTemporalResolveDualOutputDOFAware_ecd3fb1f_Wrapped.."));
        assert!(!role(b"..indirect_diffuse_lighting:CS_ScreenProbes_TemporalResolve_ReflectiveCaustics.."));
        assert!(!role(b"..TAA_Utilities:CS_Disocclusion.."));
    }

    /// What the resolve of the capture of 2026-11-27 reads, among it: other
    /// textures of the render size, a float copy of another depth and the
    /// depth target a second time.
    #[test]
    fn depth_and_vectors_among_what_the_resolve_reads() {
        let colour = Seen { format: 27, flags: 0x5, size: [1280, 720], plain: true };
        let other_depth = Seen { format: 41, flags: 0x4, size: [1280, 720], plain: true };
        let shadow = Seen { format: 19, flags: 0x2, size: [2048, 2048], plain: true };
        let read = [colour, other_depth, shadow, DEPTH, MOTION, DEPTH];
        assert_eq!(pick(&read), Some((3, 4)));
        assert_eq!(pick(&[MOTION, DEPTH]), Some((1, 0)));
        assert_eq!(pick(&[colour, DEPTH]), None, "no motion vectors");
        assert_eq!(pick(&[MOTION, other_depth, shadow]), None, "no depth target of the vectors' size");
        let slices = Seen { plain: false, ..MOTION };
        assert_eq!(pick(&[slices, DEPTH]), None);
    }

    #[test]
    fn depth_formats_read_as_floats() {
        assert_eq!(depth_view(19), Some(DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS));
        assert_eq!(depth_view(40), Some(DXGI_FORMAT_R32_FLOAT));
        // (24-bit depth is not copied as it is into a float texture here.)
        assert_eq!(depth_view(44), None);
    }

    #[test]
    fn the_pass_compiles() {
        crate::host::compile(SOURCE, s!("main"), s!("cs_5_1")).expect("the copy pass compiles");
    }
}
