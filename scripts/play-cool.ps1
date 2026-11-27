# VIscosity launcher: starts the title with the switches of cool_fox's Play-VIctory.cmd ("pareto-residency")
# plus ours for NVIDIA RTX cards (texture guard and hair strands on). Build first: cargo build --release -p victory
#   $env:VICTORY_GAME_DIR = 'D:\path\to\your\title'   (the folder with game_scarlett_master.exe), or -GameDir
#   .\play-cool.ps1                       Series S, as the launcher does
#   .\play-cool.ps1 -Console series-x     the X profile instead
#   .\play-cool.ps1 -More @{VICTORY_X='1'}  one more switch on top, for a test
#   .\play-cool.ps1 -Keys                 automatic Enter every 5 s for 4 minutes (off by default:
#                                         Enter is Cross/A, and in play the character jumps on its own)
#   .\play-cool.ps1 -Output 1080p         output size (default 'title' = the settings' 4K)
#   .\play-cool.ps1 -Scale 1.0            draw at the full output size (default: the title's 2/3; Series X only)
#   .\play-cool.ps1 -RayTracing         ray-traced indirect light on (off by default)
#   .\play-cool.ps1 -AllShaders           also the ~1,000 CS_RaytracingResolve and the ScreenProbes_ActivateRays computes
# The log goes to runs\<Name>.log in the repository, with the frame pacing every 5 s (VICTORY_TIMER_LOG):
#   bands of the times between frame beginnings; many under 28 ms = the game loop above 30 a second.
# Own shader/pipeline cache (%LOCALAPPDATA%\VIctory\cache-pareto): the first run is black for minutes.
param(
    [ValidateSet('series-s', 'series-x')][string]$Console = 'series-s',
    [string]$Name = '',
    [hashtable]$More = @{},
    [switch]$Keys,      # automatic Enter for the first 4 minutes (off: pick "Load game" yourself)
    [switch]$NoKeys,    # kept for old command lines; no Enter is now the default
    [switch]$Stats,      # the frame's cost line in the log (preparing, copies, textures, GPU, copy-back, draws)
    [switch]$RayTracing, # the title's ray-traced indirect light (VICTORY_BVH_BUILDER=1): lit interiors, about a third slower
    [switch]$AllShaders, # run the screen-probe ray and RT-resolve computes cool_fox leaves out without BVH (the stable build runs them)
    # The size the title outputs (VICTORY_OUTPUT); 'title' keeps its settings' 4K from the Series X.
    # Default 'title': at 1440p few fps were gained and artifacts showed around the player character.
    [ValidateSet('720p', '936p', '1080p', '1440p', '4k', 'title')][string]$Output = 'title',
    # The share of the output it draws before its own upscaler (VICTORY_RENDER_SCALE, 0.5 to 1.0;
    # Series X only); 0 keeps the title's 2/3 (4K output: drawn at 2560x1440).
    [double]$Scale = 0,
    # The folder with the title's executable (default: VICTORY_GAME_DIR).
    [string]$GameDir = $env:VICTORY_GAME_DIR
)
$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root 'target\release\victory.exe'
if (-not $GameDir) { Write-Host 'Set VICTORY_GAME_DIR (or -GameDir) to the folder containing your title executable.'; return }
$game = Join-Path $GameDir 'game_scarlett_master.exe'
if (-not (Test-Path $game)) { Write-Host "game_scarlett_master.exe not found in $GameDir"; return }

$settings = [ordered]@{
    VICTORY_CACHE_DIR           = "$env:LOCALAPPDATA\VIctory\cache-pareto"
    VICTORY_WAVE_WIDE_ONLY      = '5dc3b259,6f225b64,926c46ba'  # water computes get a pipeline (now upstream in cool_fox)
    VICTORY_TEXTURE_ROOM_PERCENT = '94'                         # as the stable build: more of the 16 GB card for textures (default 90)
    # From cool_fox's Play-VIctory.cmd (as of 28ebb28), unchanged:
    VICTORY_AMBIENT_AIR         = '1.5'
    VICTORY_TEXTURE_NO_WAIT     = '1'
    VICTORY_RELAY               = '2'
    VICTORY_WIDE_BACK           = '1'
    VICTORY_CARD_REHASH_RUNS    = '1'
    VICTORY_JOIN_COPIES         = '1'
    VICTORY_INDIRECT_WINDOW     = '1'
    VICTORY_RELAY_COPY          = '1'
    VICTORY_CARD_STEPS          = '2'
    VICTORY_INDIRECT_BATCH      = '2'
    VICTORY_MAP_THREADS         = '1'
    VICTORY_CARD_AWAKE          = '1'
    VICTORY_SHOW_AFTER          = '1'
    VICTORY_AMBIENT             = 'constant:0.8'
    # Ours, not the launcher's 0: textures hashed whole every eighth frame, not at every check
    # (run cool-guard-hair-1126: textures 15-21 ms / ~300 MB -> 5 ms / ~55 MB a frame).
    VICTORY_TEXTURE_GUARD       = '1'
    # Hair strands drawn: with the paired wave64 form every hair program translates
    # (26,084 of 26,084 offline) and no lines cross the screen (run cool-guard-hair-1126).
    VICTORY_HAIR                = '1'
    # Launcher of 64b8967+dacddbf: the asynchronous paths stay off on NVIDIA (with THIN the
    # head skin of people in play is not drawn; JOIN_PARTS repeats frames of the opening video).
    VICTORY_THIN                = '0'
    VICTORY_JOIN_PARTS          = '0'
    VICTORY_PORT_CONSTANTS      = '0'   # renderer ports 1 and 5 opt-in (#15: dropped roads, C++ exception)
    VICTORY_PORT_CBV            = '0'
    VICTORY_LIBRARY_STORE_LATER = '1'
    VICTORY_PIPELINE_WAIT_MS    = '0'
    VICTORY_UPLOAD_AHEAD        = '1'
    VICTORY_LEVELS_ON_DEMAND    = '1'
    VICTORY_CARD_BUFFERS_MIN_KB = '64'
    VICTORY_LOG                 = 'warn,victory_gpu::frame=info'  # warnings, plus the frame pacing lines
    VICTORY_TIMER_LOG           = '1'                             # frame pacing every 5 s
    VICTORY_PIPELINE_BOOK       = '1'   # every pipeline the title creates kept on disk, for `victory warm` (fewer stalls)
}
$sizes = @{ '720p' = '1280x720'; '936p' = '1664x936'; '1080p' = '1920x1080'; '1440p' = '2560x1440'; '4k' = '3840x2160' }
if ($Output -ne 'title') { $settings.VICTORY_OUTPUT = $sizes[$Output] }
if ($Scale -ne 0) {
    if ($Scale -lt 0.5 -or $Scale -gt 1.0) { Write-Host "-Scale: 0.5 to 1.0 (or 0 for the title's 2/3)."; return }
    if ($Console -ne 'series-x') { Write-Host "-Scale works on Series X only (Series S scales with FidelityFX); left out." }
    else { $settings.VICTORY_RENDER_SCALE = $Scale.ToString([Globalization.CultureInfo]::InvariantCulture) }
}
if ($Stats) { $settings.VICTORY_LOG = 'warn,victory_gpu::frame=info,victory_gpu::host=info' }
if ($RayTracing) { $settings.VICTORY_BVH_BUILDER = '1' }
if ($AllShaders) { $settings.VICTORY_ACTIVATE_RAYS = '1'; $settings.VICTORY_RT_RESOLVE = '1' }
foreach ($k in $More.Keys) { $settings[$k] = $More[$k] }
if ($Keys -and -not $NoKeys) { $settings.VICTORY_KEYS = (15..240 | Where-Object { $_ % 5 -eq 0 } | ForEach-Object { "${_}:Enter" }) -join ',' }

if (-not (Test-Path $exe)) { Write-Host "No $exe. Build it first."; return }
if (Get-Process victory -ErrorAction SilentlyContinue) { Write-Host 'The game is already running.'; return }
if (-not $Name) { $Name = "viscosity-" + (Get-Date -Format 'MMdd-HHmm') }
$runs = Join-Path $root 'runs'; New-Item -ItemType Directory -Force $runs | Out-Null
$log = Join-Path $runs "$Name.log"

# Only these settings: whatever VICTORY_* was left in this window goes.
Get-ChildItem Env:VICTORY_* | ForEach-Object { Remove-Item "Env:$($_.Name)" }
foreach ($k in $settings.Keys) { Set-Item "Env:$k" $settings[$k] }
Write-Host "Starting VIscosity ($Console), log $log"
Push-Location $root
try { & $exe run $game --console $Console --execute *> $log } finally { Pop-Location }
