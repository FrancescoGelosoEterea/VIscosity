//! Opt-in compatibility hooks for known title builds.
//!
//! LOCAL: copied from vitutu1/victory-perf crates/victory-loader/src/hooks.rs, which ported it from
//! ZigZagBoi1/ScarlettPort (`runtime/victory`, GPL-3.0-or-later).
//! Only per-build offsets and short byte signatures live here (asset policy:
//! nothing of the title itself). Every hook checks the build's hash and the
//! exact bytes it replaces, and does nothing otherwise.

use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// First 16 hex digits of the BLAKE3 of the Series X 1.0.2.1 executable.
const BUILD_1_0_2_1: &str = "b99f876b74d4a77f";

/// Environment variable that turns the watchdog hook on.
pub(crate) const NO_WATCHDOG_ENV: &str = "VICTORY_NO_WATCHDOG";

/// One code patch: `rva` must hold exactly `expect` before `new` is written.
struct Patch<'a> {
    what: &'static str,
    rva: usize,
    expect: &'a [u8],
    new: &'a [u8],
}

/// The title's watchdog thread ends the process on purpose when its main
/// thread has not advanced for about 30 one-second ticks. Two places do it:
/// the handler it calls (`mov dword ptr [0], imm32`) and, when the handler
/// returns or is unset, a fallback right after the call (`mov dword ptr [0],
/// 0; int3`). Jumping over the whole block returns the loop to its normal
/// path; the first patch makes the handler harmless if something else calls it.
const WATCHDOG: &[Patch<'static>] = &[
    Patch {
        what: "watchdog: skip the call and the fallback fault",
        rva: 0x2215c27,
        // mov rax, [rip + disp32] (the handler pointer)
        expect: &[0x48, 0x8B, 0x05, 0x1A, 0x07, 0xFA, 0x04],
        // jmp +0x18 (to the loop's normal continuation), then NOPs
        new: &[0xEB, 0x18, 0x90, 0x90, 0x90, 0x90, 0x90],
    },
    Patch {
        what: "watchdog: handler's deliberate fault",
        rva: 0x23cf683,
        expect: &[0xC7, 0x04, 0x25, 0, 0, 0, 0, 0x21, 0xD4, 0x8B, 0x4D],
        // an eleven-byte NOP
        new: &[0x66, 0x66, 0x2E, 0x0F, 0x1F, 0x84, 0, 0, 0, 0, 0],
    },
];

/// Applies the watchdog patches whose bytes match. Returns how many applied.
///
/// # Safety
/// `base` is the mapped image whose original BLAKE3 is `hash`, and no title
/// code is running yet.
pub(crate) unsafe fn disarm_watchdog(base: usize, hash: &blake3::Hash) -> usize {
    if !hash.to_hex().starts_with(BUILD_1_0_2_1) {
        return 0;
    }
    WATCHDOG
        .iter()
        // SAFETY: forwarded from this function's contract.
        .filter(|p| unsafe { apply(base, p) })
        .count()
}

/// Environment variable that pins the title's internal render scale
/// (`0.2..=1.0` of the output size). Testing lever, off by default.
pub(crate) const RENDER_SCALE_ENV: &str = "VICTORY_RENDER_SCALE";
/// Environment variable that makes the title use its own temporal upscaler
/// on every console profile (Series S otherwise picks FSR).
pub(crate) const OWN_UPSCALER_ENV: &str = "VICTORY_OWN_UPSCALER";

/// A patch whose bytes are computed at load time.
struct OwnedPatch {
    what: &'static str,
    rva: usize,
    expect: Vec<u8>,
    new: Vec<u8>,
}

/// The upscaler choice: `mov eax, 6` (own upscaler) then `cmovne eax, ecx`
/// (FSR when the profile asks for it). NOPing the cmovne keeps 6.
const OWN_UPSCALER: Patch<'static> = Patch {
    what: "upscaler: always the title's own",
    rva: 0x7e9426,
    expect: &[0x0F, 0x45, 0xC1],
    new: &[0x0F, 0x1F, 0x00],
};

/// Read-only float constants of this build usable as the FSR scale:
/// value, RVA.
const FSR_SCALES: [(f32, usize); 4] = [
    (0.5, 0x5fc6edc),
    (0.4, 0x5fc6f90),
    (1.0 / 3.0, 0x615a240),
    (0.75, 0x6159b04),
];

/// The render-scale patches for `scale`, with the image at `base`:
/// - the own upscaler's minimum scale (an immediate, 2/3 by default) set to
///   `scale` and its maximum made equal to the minimum (`vmaxss` → `vmovss`),
///   so the scale is fixed;
/// - FSR's scale (a load of 2/3 on Series S) pointed at a constant equal to
///   `scale`, when this build has one, and its maximum size made the scaled
///   size (vtable slot 11 → slot 10's function).
fn render_scale_patches(base: usize, scale: f32, fsr: bool) -> Vec<OwnedPatch> {
    let mut patches = vec![
        OwnedPatch {
            what: "render scale: own upscaler minimum",
            rva: 0x28f1e7e,
            expect: vec![0xAB, 0xAA, 0x2A, 0x3F],
            new: scale.to_le_bytes().to_vec(),
        },
        OwnedPatch {
            what: "render scale: own upscaler maximum = minimum",
            rva: 0x28f1e87,
            expect: vec![0xC5, 0xFA, 0x5F, 0x86, 0x90, 0, 0, 0],
            new: vec![0xC5, 0xFA, 0x10, 0x86, 0x90, 0, 0, 0],
        },
    ];
    if !fsr {
        return patches;
    }
    match FSR_SCALES.iter().find(|(v, _)| (v - scale).abs() < 1e-3) {
        Some(&(_, constant)) => {
            // vmovss xmm0, [rip + disp32]; the next instruction is at +8.
            const LOAD: usize = 0x28ef6b2;
            let disp = (constant as i64 - (LOAD as i64 + 8)) as i32;
            let mut new = vec![0xC5, 0xFA, 0x10, 0x05];
            new.extend_from_slice(&disp.to_le_bytes());
            patches.push(OwnedPatch {
                what: "render scale: FSR scale",
                rva: LOAD,
                expect: vec![0xC5, 0xFA, 0x10, 0x05, 0xA2, 0xE2, 0x86, 0x03],
                new,
            });
            patches.push(OwnedPatch {
                what: "render scale: FSR maximum = scaled size",
                rva: 0x6300ba8,
                expect: ((base + 0x28ef6f0) as u64).to_le_bytes().to_vec(),
                new: ((base + 0x28ef680) as u64).to_le_bytes().to_vec(),
            });
        }
        None => tracing::warn!(
            scale,
            "no FSR constant for this scale (0.5, 0.4, 1/3, 0.75); FSR keeps its own (use {OWN_UPSCALER_ENV}=1)"
        ),
    }
    patches
}

/// Environment variable that sets the size of the title's graphics pool on
/// the Series S profile, in megabytes (`0`: as the title has it).
pub(crate) const GRAPHICS_POOL_ENV: &str = "VICTORY_GRAPHICS_POOL_MB";

/// Where this build keeps the size of that pool for Series S, and what it
/// holds there and for Series X.
///
/// The title divides its memory into nine pools whose sizes it reads once,
/// at its start, from a table of two rows (S, X) of ten 64-bit numbers at
/// RVA 0x6186250: the total (8,572 and 13,312 MB) and the pools that add up
/// to it. The fourth number is the pool its render targets and other
/// textures of its own are placed in: 1,277 MB on S, 2,199 MB on X. Each pool
/// is one allocation from the runtime, which has no budget of its own.
///
/// A render scale above 2/3 overruns the S pool while the title makes its
/// targets: an allocation of 0x21c000 bytes fails in it and the title stops at
/// a breakpoint of its own (`+0x2219650`, seven seconds in: runs nv8 and nv25,
/// at 0.75; the same is reported of 1.0). On the console the scale never goes
/// above what the pool was sized for.
const GRAPHICS_POOL_RVA: usize = 0x6186268;
const GRAPHICS_POOL_S: u64 = 0x4fd0_0000;
const GRAPHICS_POOL_X: u64 = 0x8970_0000;

/// The megabytes the graphics pool gets: what [`GRAPHICS_POOL_ENV`] says
/// (between the title's own 1,277 and 8,192), or with nothing said the
/// Series X pool's size for a render scale above 2/3. `None`: left alone.
fn graphics_pool_mb(asked: Option<&str>, scale: Option<f32>) -> Option<u64> {
    const MIB: u64 = 1 << 20;
    match asked.map(|text| text.trim().parse::<u64>()) {
        Some(Ok(0)) => None,
        Some(Ok(mb)) if (GRAPHICS_POOL_S / MIB..=8192).contains(&mb) => Some(mb),
        Some(_) => {
            tracing::warn!("{GRAPHICS_POOL_ENV} wants megabytes between 1277 and 8192, or 0");
            None
        }
        None => scale.filter(|s| *s > 2.0 / 3.0 + 1e-3).map(|_| GRAPHICS_POOL_X / MIB),
    }
}

fn graphics_pool_patch(mb: u64) -> OwnedPatch {
    OwnedPatch {
        what: "graphics pool: Series S size",
        rva: GRAPHICS_POOL_RVA,
        expect: GRAPHICS_POOL_S.to_le_bytes().to_vec(),
        new: (mb << 20).to_le_bytes().to_vec(),
    }
}

/// The room the title keeps for the render targets that live for part of a
/// frame, in pages of 64 KiB: `mov eax, 0x1290` (Series S, 297 MB) beside
/// `mov esi, 0x2c60` (Series X, 710 MB), chosen by the console. It lays the
/// frame's 646 such targets out by their lifetimes and stops at a breakpoint
/// (`+0x2473e88`) when they need more: 453 MB at a render scale of 1.0 (runs
/// nv30 and nv31, four seconds in, once the pool above was large enough).
/// With the larger pool the Series S number is made the Series X one.
const TRANSIENT_TARGETS: Patch<'static> = Patch {
    what: "transient targets: Series S room = Series X room",
    rva: 0x1d488ac,
    expect: &[0xB8, 0x90, 0x12, 0x00, 0x00],
    new: &[0xB8, 0x60, 0x2C, 0x00, 0x00],
};

/// The scale `VICTORY_RENDER_SCALE` asks for, if valid.
fn parse_scale(text: &str) -> Option<f32> {
    let v: f32 = text.trim().parse().ok()?;
    (0.2..=1.0).contains(&v).then_some(v)
}

/// Applies the resolution levers asked for in the environment
/// ([`RENDER_SCALE_ENV`], [`OWN_UPSCALER_ENV`]). Returns how many applied.
///
/// # Safety
/// As `disarm_watchdog`.
pub(crate) unsafe fn resolution_levers(base: usize, hash: &blake3::Hash) -> usize {
    let scale = std::env::var(RENDER_SCALE_ENV).ok();
    let own = std::env::var_os(OWN_UPSCALER_ENV).is_some_and(|v| v != "0");
    let pool = std::env::var(GRAPHICS_POOL_ENV).ok();
    if scale.is_none() && !own && pool.is_none() {
        return 0;
    }
    // Every image the loader maps comes here (the title's DLLs too); only
    // the 1.0.2.1 executable has the patched places.
    if !hash.to_hex().starts_with(BUILD_1_0_2_1) {
        tracing::debug!("resolution levers: not the 1.0.2.1 executable; nothing patched");
        return 0;
    }
    let mut applied = 0;
    if own {
        // SAFETY: forwarded from this function's contract.
        applied += usize::from(unsafe { apply(base, &OWN_UPSCALER) });
    }
    if let Some(mb) = graphics_pool_mb(pool.as_deref(), scale.as_deref().and_then(parse_scale)) {
        let p = graphics_pool_patch(mb);
        let p = Patch { what: p.what, rva: p.rva, expect: &p.expect, new: &p.new };
        // SAFETY: forwarded from this function's contract; the table is read
        // by the title's start, which has not run.
        if unsafe { apply(base, &p) } {
            applied += 1;
            tracing::info!(mb, "the title's graphics pool on Series S (1277 MB as the title has it; {GRAPHICS_POOL_ENV})");
            // SAFETY: as above.
            applied += usize::from(unsafe { apply(base, &TRANSIENT_TARGETS) });
        }
    }
    if let Some(text) = scale {
        match parse_scale(&text) {
            Some(scale) => {
                // With the own upscaler forced, FSR is left as it is.
                for p in render_scale_patches(base, scale, !own) {
                    let p = Patch {
                        what: p.what,
                        rva: p.rva,
                        expect: &p.expect,
                        new: &p.new,
                    };
                    // SAFETY: forwarded from this function's contract.
                    applied += usize::from(unsafe { apply(base, &p) });
                }
            }
            None => tracing::warn!(text, "{RENDER_SCALE_ENV} wants a number in 0.2..=1.0"),
        }
    }
    applied
}

/// # Safety
/// See `disarm_watchdog`.
unsafe fn apply(base: usize, p: &Patch<'_>) -> bool {
    let at = (base + p.rva) as *mut u8;
    // SAFETY: the offset is inside the mapped .text of this exact build.
    let now = unsafe { std::slice::from_raw_parts(at, p.expect.len()) };
    if now != p.expect || p.new.len() != p.expect.len() {
        tracing::warn!(what = p.what, "hook skipped: the bytes are not the expected ones");
        return false;
    }
    let mut old = 0;
    // SAFETY: changing the protection of those bytes only, restored below.
    if unsafe { VirtualProtect(at as _, p.new.len(), PAGE_EXECUTE_READWRITE, &mut old) } == 0 {
        return false;
    }
    // SAFETY: writable now; the bytes are the ones checked above.
    unsafe {
        std::ptr::copy_nonoverlapping(p.new.as_ptr(), at, p.new.len());
        VirtualProtect(at as _, p.new.len(), old, &mut old);
        FlushInstructionCache(GetCurrentProcess(), at as _, p.new.len());
    }
    tracing::info!(what = p.what, "hook applied");
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_outside_the_floor_are_refused() {
        assert_eq!(parse_scale("0.5"), Some(0.5));
        assert_eq!(parse_scale(" 1 "), Some(1.0));
        assert_eq!(parse_scale("0.1"), None);
        assert_eq!(parse_scale("1.5"), None);
        assert_eq!(parse_scale("half"), None);
    }

    #[test]
    fn half_scale_patches_both_upscalers() {
        let base = 0x7ff0_0000_0000;
        let p = render_scale_patches(base, 0.5, true);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0].new, 0.5f32.to_le_bytes());
        // The FSR load now reads 0.5 at 0x5fc6edc: disp from the next instruction.
        assert_eq!(p[2].new, [0xC5, 0xFA, 0x10, 0x05, 0x22, 0x78, 0x6D, 0x03]);
        assert_eq!(p[3].new, ((base + 0x28ef680) as u64).to_le_bytes());
        assert!(p.iter().all(|p| p.new.len() == p.expect.len()));
    }

    #[test]
    fn the_graphics_pool_grows_for_scales_above_two_thirds_or_when_asked() {
        assert_eq!(graphics_pool_mb(None, Some(0.667)), None);
        assert_eq!(graphics_pool_mb(None, Some(0.5)), None);
        assert_eq!(graphics_pool_mb(None, None), None);
        assert_eq!(graphics_pool_mb(None, Some(0.75)), Some(2199));
        assert_eq!(graphics_pool_mb(None, Some(1.0)), Some(2199));
        assert_eq!(graphics_pool_mb(Some("0"), Some(1.0)), None);
        assert_eq!(graphics_pool_mb(Some(" 3072 "), Some(0.5)), Some(3072));
        assert_eq!(graphics_pool_mb(Some("512"), Some(1.0)), None);
        assert_eq!(graphics_pool_mb(Some("lots"), Some(1.0)), None);
        let p = graphics_pool_patch(2199);
        assert_eq!(p.expect, [0x00, 0x00, 0xD0, 0x4F, 0, 0, 0, 0]);
        assert_eq!(p.new, [0x00, 0x00, 0x70, 0x89, 0, 0, 0, 0]);
    }

    #[test]
    fn odd_scales_patch_only_the_own_upscaler() {
        assert_eq!(render_scale_patches(0, 0.6, true).len(), 2);
        assert_eq!(render_scale_patches(0, 0.5, false).len(), 2);
    }
}

