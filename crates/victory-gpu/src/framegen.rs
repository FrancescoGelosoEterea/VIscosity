//! NVIDIA DLSS Frame Generation on our present, through NVIDIA Streamline,
//! loaded at run time from a folder of the user's; off by default.
//!
//! - `VICTORY_FRAMEGEN=1`: generated frames between the title's. The device's
//!   direct queue and the swap chain are made through Streamline's proxies;
//!   every present tags the scene's depth and motion vectors, the picture
//!   without the interface, and constants made for them, and sends the
//!   latency markers frame generation needs (Reflex on). Needs the line of
//!   submissions (`VICTORY_THIN=1`).
//! - `VICTORY_FRAMEGEN_X=2..6`: frames shown for each one rendered (2).
//! - `VICTORY_STREAMLINE_DIR`: the folder of `sl.interposer.dll`, its plugins
//!   and NGX's DLLs (default [`DEFAULT_DIR`]).
//! - `VICTORY_STREAMLINE_LOG=<folder>`: Streamline's own log there, verbose.
//! - `VICTORY_FRAMEGEN_MV=x,y`: multipliers for the motion-vector scale.
//! - `VICTORY_FRAMEGEN_FOV=<radians>`: the vertical field of view told.
//! - `VICTORY_FRAMEGEN_HUDLESS=0`: no picture without the interface is tagged.
//!
//! Where the depth and the motion vectors come from: the title's own
//! temporal pass reads both ([`inputs`]), whichever upscaler follows it, so
//! nothing here needs the title's FidelityFX path. Right after that pass
//! both are copied into textures of ours, which the present after the list's
//! submission tags.
//!
//! Anything missing (the folder, a signature, the feature on this GPU, the
//! device hand-over) leaves our plain present as it was, with a warning in
//! the log.
//!
//! Streamline is used with manual hooking: nothing of D3D12 or DXGI is
//! hooked globally; only the objects handed to `slUpgradeInterface` are
//! proxies (the device that makes the direct queue, the factory that makes
//! the swap chain, and what they make).
//!
//! The library's use (loading, the device hand-over, the proxies, what a
//! present is given) follows trustno1-coder/VIctory-NVIDIA's
//! `streamline.rs` (GPL-3.0-or-later).

#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub(crate) mod abi;
pub(crate) mod inputs;
pub mod plan;

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

/// Where `sl.interposer.dll` is looked for without `VICTORY_STREAMLINE_DIR`.
pub const DEFAULT_DIR: &str = r"C:\dev\sdk\streamline-sdk-v2.14.16\bin\x64";
const INTERPOSER: &str = "sl.interposer.dll";
/// The project id NGX is given (engine "custom", no NVIDIA application id).
const PROJECT_ID: &CStr = c"6f0d7b2e-3a41-4c1e-9b57-2d8e4f1a9c30";
const ENGINE_VERSION: &CStr = c"VIctory 0.1";
/// Frame generation wants viewport 0.
const VIEWPORT: u32 = 0;
/// The title's camera planes (its FidelityFX path names them: far 10000,
/// near 0.15, depth inverted) and its field of view in play.
const NEAR: f32 = 0.15;
const FAR: f32 = 10_000.0;
const FOV_Y: f32 = 0.583;

/// What the user asked for, read from the environment once.
#[derive(Clone, Debug)]
pub struct Asked {
    pub framegen: bool,
    pub dir: PathBuf,
    /// `VICTORY_FRAMEGEN_MV`: sign or scale fixes.
    pub mvec: [f32; 2],
    /// `VICTORY_FRAMEGEN_FOV`.
    pub fov_y: f32,
    pub log_dir: Option<PathBuf>,
    pub hudless: bool,
}

impl Asked {
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let fov = var("VICTORY_FRAMEGEN_FOV").and_then(|v| v.trim().parse::<f32>().ok());
        Asked {
            framegen: var("VICTORY_FRAMEGEN").is_some_and(|v| matches!(v.trim(), "1" | "dlss")),
            dir: var("VICTORY_STREAMLINE_DIR").map_or_else(|| PathBuf::from(DEFAULT_DIR), PathBuf::from),
            mvec: plan::pair(var("VICTORY_FRAMEGEN_MV").as_deref()),
            fov_y: fov.filter(|f| *f > 0.01 && *f < 3.0).unwrap_or(FOV_Y),
            log_dir: var("VICTORY_STREAMLINE_LOG").filter(|v| !v.trim().is_empty() && v.trim() != "0").map(PathBuf::from),
            hudless: var("VICTORY_FRAMEGEN_HUDLESS").is_none_or(|v| v.trim() != "0"),
        }
    }
}

macro_rules! once {
    ($level:ident, $($arg:tt)*) => {{
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| tracing::$level!($($arg)*));
    }};
}
pub(crate) use once;

struct Library(windows_sys::Win32::Foundation::HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: this owns the reference LoadLibraryExW took.
        unsafe { windows_sys::Win32::Foundation::FreeLibrary(self.0) };
    }
}

// SAFETY: a module handle.
unsafe impl Send for Library {}
// SAFETY: as above.
unsafe impl Sync for Library {}

/// The library's exported functions.
struct Sl {
    _library: Library,
    init: abi::SlInit,
    is_feature_supported: abi::SlIsFeatureSupported,
    set_tag_for_frame: abi::SlSetTagForFrame,
    get_feature_requirements: abi::SlGetFeatureRequirements,
    get_feature_version: abi::SlGetFeatureVersion,
    upgrade_interface: abi::SlUpgradeInterface,
    set_constants: abi::SlSetConstants,
    get_native_interface: abi::SlGetNativeInterface,
    get_feature_function: abi::SlGetFeatureFunction,
    get_new_frame_token: abi::SlGetNewFrameToken,
    set_d3d_device: abi::SlSetD3DDevice,
    shutdown: abi::SlShutdown,
}

static SL: OnceLock<Sl> = OnceLock::new();
/// The switches, once the library is in and the feature is there.
static PLAN: OnceLock<Asked> = OnceLock::new();

/// The per-feature functions, once the device is set.
struct Functions {
    set_options: abi::SlDlssgSetOptions,
    get_state: abi::SlDlssgGetState,
    marker: abi::SlPclSetMarker,
}

static FUNCTIONS: OnceLock<Functions> = OnceLock::new();
/// The device was handed over and the queue is Streamline's.
static READY: AtomicBool = AtomicBool::new(false);
/// Our present goes through Streamline's swap chain.
static PROXIED: AtomicBool = AtomicBool::new(false);
/// [`check`] presents from its one thread, with nothing else submitting.
static CHECKING: AtomicBool = AtomicBool::new(false);

/// Streamline's proxies: the device that made the direct queue and the queue
/// the swap chain is made on.
struct Proxies {
    _device: ID3D12Device,
    queue: ID3D12CommandQueue,
}

// SAFETY: D3D12 objects are free-threaded.
unsafe impl Send for Proxies {}
// SAFETY: as above.
unsafe impl Sync for Proxies {}

static PROXIES: OnceLock<Proxies> = OnceLock::new();

/// What the calls at a present keep between frames, one caller at a time.
struct Calls {
    /// Frame generation is on, and the frames for each rendered one last
    /// asked of it (0 off).
    on: bool,
    times: u32,
    /// The frame whose simulation marker was sent.
    simulating: Option<u32>,
    /// The size of the inputs last tagged: a change is a cut.
    size: [u32; 2],
}

static CALLS: Mutex<Calls> = Mutex::new(Calls { on: false, times: 0, simulating: None, size: [0, 0] });

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// Since the last frame-rate line: presents with frame generation on and off,
// frames it reported shown, inputs copied; and what it last said of itself.
static ON: AtomicU64 = AtomicU64::new(0);
static OFF: AtomicU64 = AtomicU64::new(0);
static SHOWN: AtomicU64 = AtomicU64::new(0);
static COPIED: AtomicU64 = AtomicU64::new(0);
static STATUS: AtomicU32 = AtomicU32::new(0);
/// Presents with it on, and frames it showed, in all.
static ON_ALL: AtomicU64 = AtomicU64::new(0);
static SHOWN_ALL: AtomicU64 = AtomicU64::new(0);
/// Frames shown for each one presented over the last frame-rate line's
/// second, in hundredths (100 without frame generation).
static SHOWN_PER_PRESENT: AtomicU32 = AtomicU32::new(100);

/// Loads Streamline from `asked.dir` and asks for frame generation; call
/// once, before the host device is made (`crate::host::host`). Fails soft:
/// what is not there is logged and left off. Whether it will be tried.
pub fn init(asked: &Asked) -> bool {
    if !asked.framegen {
        return false;
    }
    match load_and_init(asked, true) {
        Ok(available) => available,
        Err(why) => {
            tracing::warn!(folder = %asked.dir.display(), "Streamline: {why}; frame generation stays off, our present runs as before");
            false
        }
    }
}

fn load_and_init(asked: &Asked, require_line: bool) -> Result<bool, String> {
    // Frame generation presents from the line's thread, in order with the
    // submissions: without the line it is not loaded at all.
    if require_line && !std::env::var_os("VICTORY_THIN").is_some_and(|v| v == "1") {
        return Err("DLSS Frame Generation needs the line of submissions (VICTORY_THIN=1)".into());
    }
    let path = std::path::absolute(asked.dir.join(INTERPOSER)).map_err(|e| e.to_string())?;
    if !path.is_file() {
        return Err(format!("no {INTERPOSER} in that folder (VICTORY_STREAMLINE_DIR)"));
    }
    verify_signature(&path)?;
    let sl = load(&path)?;
    let sl = SL.get_or_init(|| sl);
    let dir_wide: Vec<u16> = wide(&asked.dir);
    let paths = [dir_wide.as_ptr()];
    let log_wide = asked.log_dir.as_deref().map(wide);
    let features = [abi::FEATURE_DLSS_G, abi::FEATURE_REFLEX, abi::FEATURE_PCL];
    let preferences = abi::Preferences {
        base: abi::BaseStructure::new(abi::PREFERENCES, 1),
        show_console: false,
        log_level: if asked.log_dir.is_some() { abi::LOG_VERBOSE } else { abi::LOG_DEFAULT },
        paths_to_plugins: paths.as_ptr(),
        num_paths_to_plugins: 1,
        path_to_logs_and_data: log_wide.as_ref().map_or(std::ptr::null(), |w| w.as_ptr()),
        allocate_callback: std::ptr::null(),
        release_callback: std::ptr::null(),
        log_message_callback: Some(log_message),
        // No downloads (OTA): only what is in the folder runs.
        flags: abi::DISABLE_CL_STATE_TRACKING
            | abi::DISABLE_DEBUG_TEXT
            | abi::USE_MANUAL_HOOKING
            | abi::USE_DXGI_FACTORY_PROXY
            | abi::USE_FRAME_BASED_RESOURCE_TAGGING,
        features_to_load: features.as_ptr(),
        num_features_to_load: features.len() as u32,
        application_id: 0,
        engine: abi::ENGINE_CUSTOM,
        engine_version: ENGINE_VERSION.as_ptr(),
        project_id: PROJECT_ID.as_ptr(),
        render_api: abi::RENDER_API_D3D12,
    };
    // SAFETY: a filled preferences structure; the library copies what it keeps.
    let code = unsafe { (sl.init)(&preferences, abi::SDK_VERSION) };
    if code != 0 {
        return Err(format!("slInit failed: {} ({code})", abi::result_name(code)));
    }
    let adapter = default_adapter();
    let luid = adapter.as_ref().map(|a| a.1);
    let supported = |feature: u32| -> Result<(), String> {
        let Some(luid) = luid else { return Err("no adapter".into()) };
        let info = abi::AdapterInfo {
            base: abi::BaseStructure::new(abi::ADAPTER_INFO, 1),
            device_luid: std::ptr::from_ref(&luid).cast(),
            device_luid_size: size_of_val(&luid) as u32,
            vk_physical_device: std::ptr::null_mut(),
        };
        // SAFETY: an adapter description that lives through the call.
        let code = unsafe { (sl.is_feature_supported)(feature, &info) };
        if code == 0 { Ok(()) } else { Err(format!("{} ({code})", abi::result_name(code))) }
    };
    let framegen = report("DLSS Frame Generation", abi::FEATURE_DLSS_G, supported(abi::FEATURE_DLSS_G), sl)
        && report("Reflex", abi::FEATURE_REFLEX, supported(abi::FEATURE_REFLEX), sl);
    tracing::info!(
        library = %path.display(),
        sdk = "2.14.16",
        adapter = adapter.as_ref().map_or("?", |a| a.0.as_str()),
        framegen,
        "Streamline loaded (manual hooking, no downloads)"
    );
    if framegen {
        let _ = PLAN.set(asked.clone());
    }
    Ok(framegen)
}

/// Logs whether a feature is supported, with its requirements; whether it is.
fn report(name: &str, feature: u32, supported: Result<(), String>, sl: &Sl) -> bool {
    // SAFETY: plain data; pointers null, counts zero.
    let mut requirements: abi::FeatureRequirements = unsafe { std::mem::zeroed() };
    requirements.base = abi::BaseStructure::new(abi::FEATURE_REQUIREMENTS, 2);
    // SAFETY: a structure to fill.
    let asked = unsafe { (sl.get_feature_requirements)(feature, &mut requirements) };
    let needs = (asked == 0).then(|| {
        format!(
            "OS {} (needs {}), driver {} (needs {}){}{}",
            requirements.os_version_detected,
            requirements.os_version_required,
            requirements.driver_version_detected,
            requirements.driver_version_required,
            if requirements.flags & abi::REQUIRES_HARDWARE_SCHEDULING != 0 { ", hardware GPU scheduling" } else { "" },
            if requirements.flags & abi::REQUIRES_VSYNC_OFF != 0 { ", v-sync off" } else { "" },
        )
    });
    match supported {
        Ok(()) => {
            tracing::info!(requirements = needs.as_deref().unwrap_or("?"), "Streamline: {name} is supported on this GPU");
            true
        }
        Err(why) => {
            tracing::warn!(requirements = needs.as_deref().unwrap_or("?"), "Streamline: {name} is not supported here: {why}");
            false
        }
    }
}

unsafe extern "C" fn log_message(kind: u32, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: a NUL-terminated message for the call.
    let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    let text = text.trim_end();
    // (Every plugin in the folder that was not asked for says so.)
    let noise = text.contains("since it is was not requested by the host");
    match kind {
        abi::LOG_ERROR => tracing::error!(target: "streamline", "{text}"),
        abi::LOG_WARN if !noise => tracing::warn!(target: "streamline", "{text}"),
        _ => tracing::debug!(target: "streamline", "{text}"),
    }
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain([0]).collect()
}

/// The adapter a device made with no adapter named is on (the first), its
/// name and LUID.
fn default_adapter() -> Option<(String, windows::Win32::Foundation::LUID)> {
    // SAFETY: plain DXGI queries.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let adapter = factory.EnumAdapters1(0).ok()?;
        let desc = adapter.GetDesc1().ok()?;
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        Some((String::from_utf16_lossy(&desc.Description[..len]), desc.AdapterLuid))
    }
}

/// The Authenticode signature of `path` (NVIDIA signs its DLLs), checked
/// without going online.
fn verify_signature(path: &Path) -> Result<(), String> {
    use windows_sys::Win32::Security::WinTrust::*;
    let file = wide(path);
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: file.as_ptr(),
        hFile: std::ptr::null_mut(),
        pgKnownSubject: std::ptr::null_mut(),
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: filled structures that live through both calls.
    let code = unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, (&raw mut data).cast()) };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: as above; closes the state the first call opened.
    unsafe { WinVerifyTrust(std::ptr::null_mut(), &mut action, (&raw mut data).cast()) };
    if code == 0 {
        Ok(())
    } else {
        Err(format!("{} is not validly signed ({:#x})", path.display(), code as u32))
    }
}

fn load(path: &Path) -> Result<Sl, String> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
    };
    let file = wide(path);
    // Its own folder and System32 only, never the current folder.
    // SAFETY: a NUL-terminated full path.
    let module =
        unsafe { LoadLibraryExW(file.as_ptr(), std::ptr::null_mut(), LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32) };
    if module.is_null() {
        return Err(format!("{INTERPOSER} did not load ({})", std::io::Error::last_os_error()));
    }
    let library = Library(module);
    let find = |name: &CStr| {
        // SAFETY: a loaded module and a NUL-terminated name.
        unsafe { GetProcAddress(module, name.as_ptr().cast()) }
            .map(|f| f as *const c_void)
            .ok_or_else(|| format!("{INTERPOSER} has no {}", name.to_string_lossy()))
    };
    macro_rules! get {
        ($name:literal, $t:ty) => {
            // SAFETY: the export of that name has the header's signature.
            unsafe { std::mem::transmute::<*const c_void, $t>(find($name)?) }
        };
    }
    Ok(Sl {
        init: get!(c"slInit", abi::SlInit),
        is_feature_supported: get!(c"slIsFeatureSupported", abi::SlIsFeatureSupported),
        set_tag_for_frame: get!(c"slSetTagForFrame", abi::SlSetTagForFrame),
        get_feature_requirements: get!(c"slGetFeatureRequirements", abi::SlGetFeatureRequirements),
        get_feature_version: get!(c"slGetFeatureVersion", abi::SlGetFeatureVersion),
        upgrade_interface: get!(c"slUpgradeInterface", abi::SlUpgradeInterface),
        set_constants: get!(c"slSetConstants", abi::SlSetConstants),
        get_native_interface: get!(c"slGetNativeInterface", abi::SlGetNativeInterface),
        get_feature_function: get!(c"slGetFeatureFunction", abi::SlGetFeatureFunction),
        get_new_frame_token: get!(c"slGetNewFrameToken", abi::SlGetNewFrameToken),
        set_d3d_device: get!(c"slSetD3DDevice", abi::SlSetD3DDevice),
        shutdown: get!(c"slShutdown", abi::SlShutdown),
        _library: library,
    })
}

/// The host device is made: Streamline is given it (`slSetD3DDevice`), the
/// feature's functions are fetched, Reflex is switched on and the direct
/// queue is made through Streamline's device proxy. The native queue to use
/// for everything, if it was made here.
pub(crate) fn device_made(device: &ID3D12Device) -> Option<ID3D12CommandQueue> {
    PLAN.get()?;
    let sl = SL.get()?;
    // SAFETY: the native device.
    let code = unsafe { (sl.set_d3d_device)(device.as_raw()) };
    if code != 0 {
        tracing::warn!("Streamline: slSetD3DDevice failed: {} ({code}); frame generation stays off", abi::result_name(code));
        return None;
    }
    let function = |feature: u32, name: &CStr| -> Option<*mut c_void> {
        let mut f = std::ptr::null_mut();
        // SAFETY: a NUL-terminated name and an out pointer.
        let code = unsafe { (sl.get_feature_function)(feature, name.as_ptr(), &mut f) };
        if code != 0 || f.is_null() {
            tracing::warn!("Streamline: no {} ({}, {code})", name.to_string_lossy(), abi::result_name(code));
            return None;
        }
        Some(f)
    };
    macro_rules! f {
        ($feature:expr, $name:literal, $t:ty) => {
            // SAFETY: the function of that name has the header's signature.
            function($feature, $name).map(|f| unsafe { std::mem::transmute::<*mut c_void, $t>(f) })
        };
    }
    let set_options = f!(abi::FEATURE_DLSS_G, c"slDLSSGSetOptions", abi::SlDlssgSetOptions);
    let get_state = f!(abi::FEATURE_DLSS_G, c"slDLSSGGetState", abi::SlDlssgGetState);
    let reflex = f!(abi::FEATURE_REFLEX, c"slReflexSetOptions", abi::SlReflexSetOptions);
    let marker = f!(abi::FEATURE_PCL, c"slPCLSetMarker", abi::SlPclSetMarker);
    log_version(sl, abi::FEATURE_DLSS_G, "DLSS Frame Generation");
    log_version(sl, abi::FEATURE_REFLEX, "Reflex");
    let (Some(set_options), Some(get_state), Some(reflex), Some(marker)) = (set_options, get_state, reflex, marker) else {
        tracing::warn!("DLSS Frame Generation stays off: a function of Streamline's is missing");
        return None;
    };
    let options = abi::ReflexOptions {
        base: abi::BaseStructure::new(abi::REFLEX_OPTIONS, 1),
        mode: abi::REFLEX_LOW_LATENCY,
        frame_limit_us: 0,
        use_markers_to_optimize: false,
        virtual_key: 0,
        id_thread: 0,
    };
    // SAFETY: filled options.
    let code = unsafe { reflex(&options) };
    tracing::info!(code, result = abi::result_name(code), "Streamline: Reflex low latency on (needed by frame generation)");
    let queue = proxied_queue(sl, device);
    if queue.is_none() {
        tracing::warn!("DLSS Frame Generation stays off: the device hand-over to Streamline did not complete");
        return None;
    }
    let _ = FUNCTIONS.set(Functions { set_options, get_state, marker });
    READY.store(true, Release);
    queue
}

fn log_version(sl: &Sl, feature: u32, name: &str) {
    let mut version = abi::FeatureVersion {
        base: abi::BaseStructure::new(abi::FEATURE_VERSION, 1),
        version_sl: abi::Version::default(),
        version_ngx: abi::Version::default(),
    };
    // SAFETY: a structure to fill.
    let code = unsafe { (sl.get_feature_version)(feature, &mut version) };
    if code == 0 {
        tracing::info!(streamline = %version.version_sl, ngx = %version.version_ngx, "Streamline: {name} loaded");
    } else {
        tracing::warn!("Streamline: {name} has no version: {} ({code})", abi::result_name(code));
    }
}

/// The direct queue, made through Streamline's proxy of `device` (frame
/// generation hooks queue creation); the native queue is returned for our
/// use and the proxy kept for the swap chain.
fn proxied_queue(sl: &Sl, device: &ID3D12Device) -> Option<ID3D12CommandQueue> {
    let mut raw = device.as_raw();
    // SAFETY: an owned reference of ours; on success `raw` is a new proxy
    // that holds its own reference to the device.
    let code = unsafe { (sl.upgrade_interface)(&mut raw) };
    if code != 0 || raw.is_null() || raw == device.as_raw() {
        tracing::warn!("Streamline: the device was not upgraded: {} ({code})", abi::result_name(code));
        return None;
    }
    // SAFETY: the proxy's one reference, now ours.
    let proxy_device = unsafe { ID3D12Device::from_raw(raw) };
    let desc = D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() };
    // SAFETY: plain queue creation through the proxy.
    let proxy_queue: ID3D12CommandQueue = match unsafe { proxy_device.CreateCommandQueue(&desc) } {
        Ok(q) => q,
        Err(e) => {
            tracing::warn!(error = %e, "Streamline: no queue from the device proxy");
            return None;
        }
    };
    let mut native = std::ptr::null_mut();
    // SAFETY: a proxy queue; the native one comes back with a reference.
    let code = unsafe { (sl.get_native_interface)(proxy_queue.as_raw(), &mut native) };
    if code != 0 || native.is_null() {
        tracing::warn!("Streamline: no native queue behind the proxy: {} ({code})", abi::result_name(code));
        return None;
    }
    // SAFETY: the reference slGetNativeInterface added.
    let native = unsafe { ID3D12CommandQueue::from_raw(native) };
    tracing::info!("Streamline: the direct queue was made through the device proxy (frame generation)");
    let _ = PROXIES.set(Proxies { _device: proxy_device, queue: proxy_queue });
    Some(native)
}

/// Streamline's proxy of `factory` and the queue proxy to make the swap
/// chain with; `None` to make it as before.
pub(crate) fn swap_chain_factory(factory: &IDXGIFactory2) -> Option<(IDXGIFactory2, ID3D12CommandQueue)> {
    if !READY.load(Acquire) {
        return None;
    }
    let sl = SL.get()?;
    let queue = PROXIES.get()?.queue.clone();
    let mut raw = factory.as_raw();
    // SAFETY: as for the device: a new proxy with its own reference.
    let code = unsafe { (sl.upgrade_interface)(&mut raw) };
    if code != 0 || raw.is_null() || raw == factory.as_raw() {
        tracing::warn!("Streamline: the DXGI factory was not upgraded: {} ({code})", abi::result_name(code));
        READY.store(false, Release);
        return None;
    }
    // SAFETY: the proxy's reference, now ours.
    let proxy = unsafe { IDXGIFactory2::from_raw(raw) };
    Some((proxy, queue))
}

/// The swap chain was made through the proxy (or not, after all: then frame
/// generation stays off).
pub(crate) fn swap_chain_made(proxied: bool, error: Option<&windows::core::Error>) {
    PROXIED.store(proxied, Release);
    if proxied {
        tracing::info!("Streamline: our swap chain is Streamline's proxy; DLSS Frame Generation can run on its presents");
    } else {
        READY.store(false, Release);
        tracing::warn!(
            error = error.map(|e| e.to_string()).unwrap_or_default(),
            "Streamline: the swap chain could not be made through the proxy; made as before, frame generation off"
        );
    }
}

/// Whether frame generation runs on our presents: the title's temporal pass
/// is then followed for its depth and motion vectors.
#[inline]
pub(crate) fn wanted() -> bool {
    PROXIED.load(Relaxed) && READY.load(Relaxed)
}

/// Whether the picture without the interface is to be tagged.
pub(crate) fn hudless_wanted() -> bool {
    wanted() && PLAN.get().is_some_and(|p| p.hudless)
}

/// The depth and motion vectors of a frame, in textures of ours.
struct Noted {
    id: u64,
    depth: Tagged,
    motion: Tagged,
    size: [u32; 2],
}

/// A texture frame generation reads at the present, in a state it stays in.
#[derive(Clone)]
pub(crate) struct Tagged(pub ID3D12Resource, pub u32);

// SAFETY: D3D12 resources are free-threaded.
unsafe impl Send for Tagged {}

impl Tagged {
    fn resource(&self) -> abi::Resource {
        abi::Resource::texture(self.0.as_raw(), self.1)
    }
}

/// Copies recorded in lists not submitted yet.
static PENDING: Mutex<Vec<Noted>> = Mutex::new(Vec::new());
/// The latest copy submitted, and the one the last present tagged.
static SUBMITTED: Mutex<Option<Noted>> = Mutex::new(None);
static PRESENTED_ID: AtomicU64 = AtomicU64::new(0);
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// The number of the next copy of the inputs.
pub(crate) fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Relaxed) + 1
}

/// Copy `id` of a frame's depth and motion vectors was recorded into a list
/// (which is marked with it: [`submitted`] makes it the latest).
pub(crate) fn noted(id: u64, depth: Tagged, motion: Tagged, size: [u32; 2]) {
    let mut pending = lock(&PENDING);
    pending.retain(|n| id.saturating_sub(n.id) < 16);
    pending.push(Noted { id, depth, motion, size });
    COPIED.fetch_add(1, Relaxed);
}

/// A host list that recorded copy `id` is submitted (in submission order,
/// before the present that follows it).
pub(crate) fn submitted(id: u64) {
    let noted = {
        let mut pending = lock(&PENDING);
        let at = pending.iter().position(|n| n.id == id);
        at.map(|i| pending.swap_remove(i))
    };
    if let Some(noted) = noted {
        let mut latest = lock(&SUBMITTED);
        if latest.as_ref().is_none_or(|l| l.id < noted.id) {
            *latest = Some(noted);
        }
    }
}

fn frame_token(sl: &Sl, index: u32) -> Option<*mut abi::FrameToken> {
    let mut token = std::ptr::null_mut();
    // SAFETY: an out pointer and an index.
    let code = unsafe { (sl.get_new_frame_token)(&mut token, &index) };
    (code == 0 && !token.is_null()).then_some(token)
}

/// What a present shows besides the back buffer: the picture without the
/// interface (a texture of the back buffer's size and the host state it is
/// in), if there is one, and the back buffer's size.
pub(crate) struct Shown<'a> {
    pub hudless: Option<(&'a ID3D12Resource, u32)>,
    pub size: [u32; 2],
}

/// What [`present_begins`] began, for [`present_ends`].
pub(crate) struct Presenting {
    token: *mut abi::FrameToken,
    frame: u32,
    called: std::time::Instant,
}

/// The format a typeless texture is read in, for Streamline (0: its own).
fn read_as(texture: &ID3D12Resource) -> u32 {
    // SAFETY: a live texture.
    match unsafe { texture.GetDesc() }.Format.0 {
        27 => 28,
        90 => 87,
        _ => 0,
    }
}

/// Before our swap chain's Present of title frame `frame`: the frame's
/// latency markers, constants and tags, and whether frame generation runs on
/// it (it does when a copy of the scene's depth and motion vectors was
/// submitted since the last present).
pub(crate) fn present_begins(frame: u64, shown: Shown<'_>) -> Option<Presenting> {
    if !wanted() {
        return None;
    }
    // The copies are complete when the submissions before this present have
    // been sent: only the line of submissions orders the two.
    if !crate::queue::line_on() && !CHECKING.load(Relaxed) {
        once!(warn, "DLSS Frame Generation: the line of submissions did not start; frame generation stays off");
        return None;
    }
    let (sl, functions, plan) = (SL.get()?, FUNCTIONS.get()?, PLAN.get()?);
    let began = std::time::Instant::now();
    let frame = frame as u32;
    let token = frame_token(sl, frame)?;
    // SAFETY: markers for a token of ours.
    let mark = |m: u32| unsafe { (functions.marker)(m, token) };
    let mut calls = lock(&CALLS);
    if calls.simulating != Some(frame) {
        mark(abi::SIMULATION_START);
    }
    calls.simulating = None;
    mark(abi::SIMULATION_END);
    mark(abi::RENDER_SUBMIT_START);
    mark(abi::RENDER_SUBMIT_END);
    let fresh = {
        let latest = lock(&SUBMITTED);
        latest.as_ref().filter(|n| n.id > PRESENTED_ID.load(Relaxed)).map(|n| (n.id, n.depth.clone(), n.motion.clone(), n.size))
    };
    let viewport = abi::ViewportHandle::new(VIEWPORT);
    let tagged = if let Some((id, depth, motion, size)) = fresh {
        PRESENTED_ID.store(id, Relaxed);
        let camera = plan::Camera {
            render: size,
            near: NEAR,
            far: FAR,
            fov_y: plan.fov_y,
            depth_inverted: true,
            // Another size, or the first frame after a stop: a cut.
            reset: calls.size != size || !calls.on,
        };
        calls.size = size;
        let constants = camera.constants(plan.mvec);
        // SAFETY: constants for this frame and viewport.
        let code = unsafe { (sl.set_constants)(&constants, token, &viewport) };
        if code != 0 {
            once!(warn, "frame generation: slSetConstants: {} ({code})", abi::result_name(code));
        }
        let mut resources = vec![(depth.resource(), abi::BUFFER_DEPTH, size), (motion.resource(), abi::BUFFER_MOTION_VECTORS, size)];
        let hudless = shown.hudless.filter(|_| plan.hudless);
        if let Some((texture, state)) = hudless {
            let mut resource = abi::Resource::texture(texture.as_raw(), state);
            resource.native_format = read_as(texture);
            resources.push((resource, abi::BUFFER_HUDLESS_COLOR, shown.size));
        }
        let tags: Vec<abi::ResourceTag> = resources
            .iter_mut()
            .map(|(r, kind, size)| abi::ResourceTag::new(r, *kind, abi::VALID_UNTIL_PRESENT, abi::Extent::sized(*size)))
            .collect();
        // SAFETY: tags of live textures in the states they are in at the
        // present (the line of submissions sends the present after them).
        let code = unsafe { (sl.set_tag_for_frame)(token, &viewport, tags.as_ptr(), tags.len() as u32, std::ptr::null_mut()) };
        if code != 0 {
            once!(warn, "frame generation: slSetTagForFrame: {} ({code})", abi::result_name(code));
        }
        once!(
            info,
            inputs = ?size,
            back_buffer = ?shown.size,
            without_interface = hudless.is_some(),
            near = constants.camera_near,
            far = constants.camera_far,
            fov = constants.camera_fov,
            mvec_scale = ?constants.mvec_scale,
            "DLSS Frame Generation: first frame with depth and motion vectors tagged"
        );
        code == 0
    } else {
        if calls.on {
            // No copy this frame (a menu, a load, a video): what was tagged
            // no longer belongs to what is shown.
            let tags = [abi::BUFFER_DEPTH, abi::BUFFER_MOTION_VECTORS, abi::BUFFER_HUDLESS_COLOR]
                .map(|kind| abi::ResourceTag::new(std::ptr::null_mut(), kind, abi::VALID_UNTIL_PRESENT, abi::Extent::default()));
            // SAFETY: null tags.
            unsafe { (sl.set_tag_for_frame)(token, &viewport, tags.as_ptr(), tags.len() as u32, std::ptr::null_mut()) };
        }
        false
    };
    let wanted = times_asked();
    let on = tagged && wanted != 0;
    let times = if on { wanted } else { 0 };
    if on != calls.on || times != calls.times {
        let mut options = abi::DlssgOptions::new(if on { abi::DLSSG_ON } else { abi::DLSSG_OFF });
        // No more than this card and driver generate (the state says how many).
        let mut generated = wanted.max(2) - 1;
        if on && generated > 1 {
            let mut state = abi::DlssgState::default();
            // SAFETY: a structure to fill, no options.
            let code = unsafe { (functions.get_state)(&viewport, &mut state, std::ptr::null()) };
            let most = if code == 0 { state.num_frames_to_generate_max.max(1) } else { 1 };
            if generated > most {
                tracing::warn!(asked = generated + 1, given = most + 1, "DLSS Frame Generation: more frames asked than this card generates");
                generated = most;
            }
        }
        options.num_frames_to_generate = generated;
        options.flags = abi::DLSSG_RETAIN_RESOURCES_WHEN_OFF;
        options.num_back_buffers = 2;
        options.color_width = shown.size[0];
        options.color_height = shown.size[1];
        options.on_error_callback = Some(present_error);
        // SAFETY: filled options for viewport 0, on the presenting thread.
        let code = unsafe { (functions.set_options)(&viewport, &options) };
        tracing::info!(frame, on, times = generated + 1, code, result = abi::result_name(code), "DLSS Frame Generation switched");
        // 39, "warning: out of VRAM": the options are set all the same.
        if code == 39 {
            tracing::warn!("DLSS Frame Generation is on with the card's memory past its budget: frames may come unevenly");
        }
        if code == 0 || code == 39 {
            calls.on = on;
            calls.times = times;
            if !on {
            }
        }
    }
    if calls.on {
        ON.fetch_add(1, Relaxed);
        let presents = ON_ALL.fetch_add(1, Relaxed) + 1;
        if (presents == 1 || presents == 240)
            && !CHECKING.load(Relaxed)
            && let Some(host) = crate::host::host()
        {
            host.log_video_memory(if presents == 1 {
                "right before the first present with frame generation on"
            } else {
                "240 presents with frame generation on later"
            });
        }
    } else {
        OFF.fetch_add(1, Relaxed);
    }
    drop(calls);
    mark(abi::PRESENT_START);
    TIMES[0].fetch_add(began.elapsed().as_micros() as u64, Relaxed);
    Some(Presenting { token, frame, called: std::time::Instant::now() })
}

// Since the last frame-rate line, microseconds on the presenting thread:
// before the present call, in it (and the longest), after it.
static TIMES: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

unsafe extern "C" fn present_error(error: *const i32) {
    // SAFETY: the error for the call.
    let code = unsafe { error.as_ref() }.copied().unwrap_or(0);
    once!(warn, hresult = format_args!("{:#x}", code as u32), "DLSS Frame Generation: a present failed");
}

/// Frames shown for each one rendered: `VICTORY_FRAMEGEN_X=2..6` (2 unless
/// set).
fn times_asked() -> u32 {
    static TIMES: OnceLock<u32> = OnceLock::new();
    *TIMES.get_or_init(|| std::env::var("VICTORY_FRAMEGEN_X").ok().and_then(|v| v.trim().parse::<u32>().ok()).map_or(2, |n| n.clamp(2, 6)))
}

/// Frames frame generation is asked to show for each one presented now (1
/// while it is off): what a cap on the frames shown divides by.
pub(crate) fn shown_for_each() -> u32 {
    let calls = lock(&CALLS);
    if calls.on { calls.times.max(1) } else { 1 }
}

/// For the frame-rate texts: frames shown for each one rendered over the
/// last measured second, in hundredths (frame generation leaves out a frame
/// that would come too late, so this is what was shown, not what was asked).
pub(crate) fn shown_hundredths() -> u32 {
    SHOWN_PER_PRESENT.load(Relaxed).max(100)
}

/// After our swap chain's Present: the frame's last marker, the next frame's
/// first, and what frame generation reports.
pub(crate) fn present_ends(presenting: Option<Presenting>) {
    let Some(Presenting { token, frame, called }) = presenting else { return };
    let took = called.elapsed().as_micros() as u64;
    TIMES[1].fetch_add(took, Relaxed);
    TIMES[2].fetch_max(took, Relaxed);
    let (Some(sl), Some(functions)) = (SL.get(), FUNCTIONS.get()) else { return };
    let began = std::time::Instant::now();
    // SAFETY: a marker for this frame's token.
    unsafe { (functions.marker)(abi::PRESENT_END, token) };
    let mut calls = lock(&CALLS);
    if let Some(next) = frame_token(sl, frame.wrapping_add(1)) {
        // SAFETY: as above, for the next frame.
        unsafe { (functions.marker)(abi::SIMULATION_START, next) };
        calls.simulating = Some(frame.wrapping_add(1));
    }
    if calls.on {
        let mut state = abi::DlssgState::default();
        let viewport = abi::ViewportHandle::new(VIEWPORT);
        // SAFETY: a structure to fill, no options (no estimate asked).
        let code = unsafe { (functions.get_state)(&viewport, &mut state, std::ptr::null()) };
        if code == 0 {
            SHOWN.fetch_add(u64::from(state.num_frames_actually_presented), Relaxed);
            SHOWN_ALL.fetch_add(u64::from(state.num_frames_actually_presented), Relaxed);
            let before = STATUS.swap(state.status, Relaxed);
            if state.status != before {
                tracing::warn!(
                    status = format_args!("{:#x}", state.status),
                    "DLSS Frame Generation status changed (0 ok, 1 output too small, 2 Reflex not seen, 4 HDR format, 8 constants invalid, 16 back buffer index not asked)"
                );
            }
        }
    }
    TIMES[3].fetch_add(began.elapsed().as_micros() as u64, Relaxed);
}

/// Our swap chain's buffers are about to be made again: frame generation is
/// off until the next present with inputs (Streamline asks so).
pub(crate) fn resizing() {
    if !wanted() {
        return;
    }
    let Some(functions) = FUNCTIONS.get() else { return };
    let mut calls = lock(&CALLS);
    if calls.on {
        let options = abi::DlssgOptions::new(abi::DLSSG_OFF);
        let viewport = abi::ViewportHandle::new(VIEWPORT);
        // SAFETY: filled options.
        unsafe { (functions.set_options)(&viewport, &options) };
        calls.on = false;
        calls.times = 0;
    }
}

/// For the frame-rate line.
pub(crate) fn line(frames: f64) {
    if PLAN.get().is_none() {
        return;
    }
    let [on, off, shown, copied] = [&ON, &OFF, &SHOWN, &COPIED].map(|c| c.swap(0, Relaxed));
    // (A present with it off, or one it showed nothing more for, is one frame.)
    let presents = on + off;
    let hundredths = ((shown.max(on) + off) * 100).checked_div(presents).unwrap_or(100);
    SHOWN_PER_PRESENT.store(hundredths.clamp(100, 1000) as u32, Relaxed);
    let t: Vec<u64> = TIMES.iter().map(|t| t.swap(0, Relaxed)).collect();
    let ms = |slot: usize| t[slot] as f64 / 1000.0 / frames.max(1.0);
    tracing::info!(
        "DLSS Frame Generation: {on} presents with it on, {off} off, {shown} frames shown by it, {copied} copies of the scene's depth and motion vectors (status {:#x}); the presenting thread {:.2} ms before the present call, {:.2} ms in it (the longest {:.1} ms), {:.2} ms after it{}",
        STATUS.load(Relaxed),
        ms(0),
        ms(1),
        t[2] as f64 / 1000.0,
        ms(3),
        if wanted() { "" } else { "; not running (see above)" }
    );
}

/// `victory framegen-check`: Streamline and DLSS Frame Generation on this PC,
/// without the title. Always: the library loads (signature, `slInit`) and
/// says whether this GPU supports it. With `present`: the host device is
/// made and handed over, then three seconds of presents in a window of ours
/// with stand-in depth and motion vectors copied and tagged as the title's
/// are, frame generation on. The lines say what happened.
pub fn check(output: [u32; 2], present: bool) -> Vec<String> {
    let mut lines = check_run(output, present);
    lines.extend(shut_down());
    lines
}

/// The host device was made for [`check`].
static CHECK_DEVICE: AtomicBool = AtomicBool::new(false);

/// The end of [`check`] with a device: frame generation off, our swap chain
/// let go, Streamline shut down, before the process ends (without it the
/// process fails fast at its exit with frame generation's swap chain up).
fn shut_down() -> Option<String> {
    if !CHECK_DEVICE.load(Relaxed) {
        return None;
    }
    let (sl, host) = (SL.get()?, crate::host::host()?);
    resizing();
    host.drop_presenter();
    READY.store(false, Release);
    PROXIED.store(false, Release);
    *lock(&SUBMITTED) = None;
    lock(&PENDING).clear();
    inputs::let_go();
    // SAFETY: initialised; nothing of it is in use or called from here on.
    let code = unsafe { (sl.shutdown)() };
    Some(format!("Streamline shut down: {} ({code})", abi::result_name(code)))
}

fn check_run(output: [u32; 2], present: bool) -> Vec<String> {
    let mut lines = Vec::new();
    let mut asked = Asked::from_env();
    asked.framegen = true;
    let available = match load_and_init(&asked, false) {
        Ok(available) => available,
        Err(why) => return vec![format!("Streamline did not load from {}: {why}", asked.dir.display())],
    };
    lines.push(format!(
        "Streamline loaded from {}: DLSS Frame Generation {} on this GPU",
        asked.dir.display(),
        if available { "supported" } else { "NOT supported" }
    ));
    if !present {
        return lines;
    }
    let Some(host) = crate::host::host() else {
        lines.push("no host D3D12 device".into());
        return lines;
    };
    CHECK_DEVICE.store(true, Relaxed);
    lines.push(format!("device handed over: frame generation ready {} (queue through Streamline's device proxy)", READY.load(Acquire)));
    if !READY.load(Acquire) {
        return lines;
    }
    let device = &host.device;
    let made = (|| -> windows::core::Result<_> {
        use windows::Win32::Graphics::Dxgi::Common::*;
        // As the title's: a depth target with a stencil plane and 16-bit
        // float vectors, both readable by shaders.
        let depth = texture(device, output, DXGI_FORMAT_R32G8X24_TYPELESS, D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL, SHADER_READ)?;
        let motion = texture(device, output, DXGI_FORMAT_R16G16_FLOAT, D3D12_RESOURCE_FLAG_NONE, SHADER_READ)?;
        let base = texture(device, output, DXGI_FORMAT_R8G8B8A8_UNORM, D3D12_RESOURCE_FLAG_NONE, 0)?;
        Ok((depth, motion, base))
    })();
    let (depth, motion, base) = match made {
        Ok(made) => made,
        Err(e) => {
            lines.push(format!("stand-in textures not made: {e}"));
            return lines;
        }
    };
    CHECKING.store(true, Relaxed);
    let copies = NEXT_ID.load(Relaxed);
    for frame in 1..=180u64 {
        // The first present makes the swap chain; from then on the inputs
        // are copied in a list of ours, as after the title's temporal pass.
        if wanted() {
            let list = match host.new_list() {
                Ok(list) => list,
                Err(e) => {
                    lines.push(format!("no command list: {e}"));
                    return lines;
                }
            };
            let id = inputs::record(&list, &depth, &motion);
            list.close();
            host.execute(&[&list]);
            match id {
                Some(id) => submitted(id),
                None => {
                    lines.push("the copy of the depth and the motion vectors was not recorded".into());
                    return lines;
                }
            }
        }
        host.present_over(&base, 0, None, frame);
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    crate::host::settle_for_fence();
    lines.push(format!(
        "frame generation: 180 presents, {} with it on, {} frames shown by it, {} copies of depth and motion vectors, status {:#x} (0 is fine){}",
        ON_ALL.load(Relaxed),
        SHOWN_ALL.load(Relaxed),
        NEXT_ID.load(Relaxed) - copies,
        STATUS.load(Relaxed),
        if PROXIED.load(Relaxed) { "" } else { "; the swap chain is NOT Streamline's proxy" }
    ));
    // SAFETY: a plain query.
    if let Err(e) = unsafe { device.GetDeviceRemovedReason() } {
        lines.push(format!("the device was removed while presenting: {e}"));
    }
    lines
}

const SHADER_READ: u32 = 0xc0;

/// A texture of ours (its contents are whatever the memory held).
pub(crate) fn texture(
    device: &ID3D12Device,
    size: [u32; 2],
    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT,
    flags: D3D12_RESOURCE_FLAGS,
    state: u32,
) -> windows::core::Result<ID3D12Resource> {
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: u64::from(size[0]),
        Height: size[1],
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: format,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: flags,
    };
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    let mut made: Option<ID3D12Resource> = None;
    // SAFETY: plain texture creation.
    unsafe {
        device.CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATES(state as i32), None, &mut made)?
    };
    made.ok_or_else(windows::core::Error::empty)
}
