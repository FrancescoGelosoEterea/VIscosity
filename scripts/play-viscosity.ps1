# VIscosity launcher: miamiwalk's settings (scripts\play-miamiwalk.ps1, the world drawn at 1920x1080 on the
# Series S profile) for this tree, without local paths, and with the title's watchdog off.
#   $env:VICTORY_GAME_DIR = 'D:\path\to\your\title'   (the folder with game_scarlett_master.exe), or -GameDir
#   scripts\play-viscosity.ps1                     the world at 1080p, in a 1920x1080 window
#   scripts\play-viscosity.ps1 -Fullscreen         borderless over the whole monitor
#   scripts\play-viscosity.ps1 -World 720          the world at 1280x720, stretched by the title's upscaler
#   scripts\play-viscosity.ps1 -More @{VICTORY_X='1'}   one more switch on top
#   scripts\play-viscosity.ps1 -FrameGen           NVIDIA DLSS Frame Generation (RTX 40 or later): two frames
#                                                  shown for each one rendered; -FrameGenX 3 to 6 for more.
#                                                  Needs NVIDIA's Streamline files (sl.interposer.dll and the
#                                                  rest of the SDK's bin\x64) in nvidia-sdk\Streamline\bin\x64
#                                                  here or beside this tree in VIctory-NVIDIA, or -Streamline <folder>.
#   scripts\play-viscosity.ps1 -HeatHaze           keep the title's heat-haze pass (left out: on NVIDIA it paints
#                                                  black blotches where hot air is, as trails in the sky)
#   scripts\play-viscosity.ps1 -Cap 30            at most that many frames shown a second (30 unless said, 0: no cap):
#                                                  presents are spaced evenly, a steadier picture than every frame
#                                                  as it comes. With -FrameGen at two for one: 15 rendered a second.
#   scripts\play-viscosity.ps1 -Freezes           writes what the layer was doing around each frame of 200 ms or more
#                                                  (up to 40 of them, 20 s apart, from -FreezeFrom seconds in: 60) to
#                                                  runs\<Name>-freeze.*.txt/json
#   scripts\play-viscosity.ps1 -Mouse             the mouse as the right stick of the keyboard's stand-in pad. The cursor
#                                                  is then held at the window's centre while the game has the focus;
#                                                  without the switch the mouse is left alone.
#   scripts\play-viscosity.ps1 -Prepare           does not start the title: fills the shader cache for these settings
#                                                  and makes the pipelines earlier runs wrote down. Run it once before
#                                                  the first start, and again after a start with
#                                                  -More @{VICTORY_PIPELINE_BOOK='1'} (which writes the pipelines down).
# Build first: cargo build --locked --profile diag -p victory --features diagnostics
# The log goes to runs\<Name>.log. The first run of a place is slow while shaders are translated.
param(
    [string]$GameDir = $env:VICTORY_GAME_DIR,
    [ValidateSet('720', '810', '1080')][string]$World = '1080',
    [switch]$Fullscreen,
    [string]$Window = '1936x1119',
    [string]$Name = ("viscosity-" + (Get-Date -Format 'MMdd-HHmm')),
    [switch]$FrameGen,
    [ValidateRange(2, 6)][int]$FrameGenX = 2,
    [switch]$HeatHaze,
    [ValidateRange(0, 240)][int]$Cap = 30,
    [switch]$Freezes,
    [switch]$Prepare,
    [switch]$Mouse,
    [ValidateRange(50, 5000)][int]$FreezeMs = 200,
    [ValidateRange(0, 100000)][int]$FreezeFrom = 60,
    [string]$Streamline,
    [hashtable]$More = @{}
)
if (-not $GameDir) { Write-Host 'Set VICTORY_GAME_DIR (or -GameDir) to the folder containing your title executable.'; return }
$exe = Join-Path $GameDir 'game_scarlett_master.exe'
if (-not (Test-Path $exe)) { Write-Host "game_scarlett_master.exe not found in $GameDir"; return }
if (Get-Process victory -ErrorAction SilentlyContinue) { Write-Host 'The game is already running.'; return }
$root = Split-Path $PSScriptRoot -Parent
$env:VICTORY_CACHE_DIR = "$env:LOCALAPPDATA\VIctory\cache-nvidia"
$env:VICTORY_GAME_DIR = $GameDir
# As play-miamiwalk.ps1 (see its header for why each is set):
$settings = @{
    VICTORY_CARD_BUFFERS_MB = '3072'; VICTORY_CARD_BUFFERS_MIN_KB = '64'
    VICTORY_SHARPEN = '0.6'; VICTORY_BURY_LAG = '24'
    VICTORY_RECHECK_FRAMES = '2,4,8,16,32,64,128,256'; VICTORY_RECHECK_REFILLS = '4'
    VICTORY_DISPLAY_MODE = '2560x1440'
    # The title's watchdog ends the process (a write to 0 at +0x2215c35) when its loop stands still too long:
    # on a cold cache, 29 s of waiting for new pipelines (PIPELINE_WAIT_MS, 10 s each) in the menu did it.
    VICTORY_NO_WATCHDOG = '1'
    # Glass: pixel programs whose console code tests depth before they run get the host's early depth test
    # (victory-shader emit: without it glass behind hair takes the hair's place, and windows smear). Part of the
    # shader cache's key: after changing it, run `victory shaders` and `victory warm --used` once.
    VICTORY_EARLY_DEPTH = '1'
}
if ($Fullscreen) { $settings.VICTORY_FULLSCREEN = '1' } else { $settings.VICTORY_WINDOW_SIZE = $Window }
$settings.VICTORY_RENDER_SCALE = @{ '720' = '0.667'; '810' = '0.75'; '1080' = '1.0' }[$World]
if ($World -eq '1080') { $settings.VICTORY_SHARPEN = '0.3' }
if ($FrameGen) {
    # DLSS Frame Generation on our present (victory-gpu framegen.rs): the title keeps its own upscaler.
    $folders = @($Streamline, (Join-Path $root 'nvidia-sdk\Streamline\bin\x64'), (Join-Path $root '..\VIctory-NVIDIA\nvidia-sdk\Streamline\bin\x64')) | Where-Object { $_ }
    $found = $folders | Where-Object { Test-Path (Join-Path $_ 'sl.interposer.dll') } | Select-Object -First 1
    if (-not $found) { Write-Host "No sl.interposer.dll in: $($folders -join ', '). Frame generation needs NVIDIA's Streamline files."; return }
    $settings.VICTORY_FRAMEGEN = '1'
    $settings.VICTORY_FRAMEGEN_X = "$FrameGenX"
    $settings.VICTORY_STREAMLINE_DIR = (Resolve-Path $found).Path
}
if ($Cap -gt 0) { $settings.VICTORY_SHOWN_CAP = "$Cap" }
# Mouse-look puts the cursor back at the window's centre at every poll (victory-hle gameinput.rs): off unless asked.
if (-not $Mouse) { $settings.VICTORY_MOUSE = '0' }
$runs = Join-Path $root 'runs'; New-Item -ItemType Directory -Force $runs | Out-Null
if ($Freezes) {
    # The frame timeline (victory-gpu timeline.rs), kept only around frames that take long.
    $settings.VICTORY_TIMELINE = Join-Path $runs "$Name-freeze.json"
    $settings.VICTORY_TIMELINE_FREEZE = "$FreezeMs"
    $settings.VICTORY_TIMELINE_MOST = '40'; $settings.VICTORY_TIMELINE_APART = '20'; $settings.VICTORY_TIMELINE_AT = "$FreezeFrom"
}
foreach ($k in $More.Keys) { $settings[$k] = $More[$k] }
$log = Join-Path $runs "$Name.log"
Write-Host "Starting VIscosity (world $World), log $log"
# scattering_transmittance_system:CS_IndirectApplyDiffusionInScattering_HeatHaze (see -HeatHaze above).
$skip = @(); if (-not $HeatHaze) { $skip += '731a39171b568298' }
# The log is written by the layer itself (VICTORY_LOG_FILE): through a redirected stream every line waits for
# PowerShell, and so do the threads that log. What else comes on that stream (a panic) goes to <Name>.err.log.
$settings.VICTORY_LOG_FILE = $log
if ($Prepare) {
    $settings.Remove('VICTORY_LOG_FILE'); $settings.Remove('VICTORY_TIMELINE')
    & "$PSScriptRoot\play-nvidia.ps1" -Exe $exe -NoSmoothMotion -Build diag -Skip $skip -More $settings -Prepare
    return
}
& "$PSScriptRoot\play-nvidia.ps1" -Exe $exe -NoSmoothMotion -Build diag -Skip $skip -More $settings 2> (Join-Path $runs "$Name.err.log")
