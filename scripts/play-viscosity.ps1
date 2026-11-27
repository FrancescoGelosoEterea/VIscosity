# VIscosity launcher: miamiwalk's settings (scripts\play-miamiwalk.ps1, the world drawn at 1920x1080 on the
# Series S profile) for this tree, without local paths, and with the title's watchdog off.
#   $env:VICTORY_GAME_DIR = 'D:\path\to\your\title'   (the folder with game_scarlett_master.exe), or -GameDir
#   scripts\play-viscosity.ps1                     the world at 1080p, in a 1920x1080 window
#   scripts\play-viscosity.ps1 -Fullscreen         borderless over the whole monitor
#   scripts\play-viscosity.ps1 -World 720          the world at 1280x720, stretched by the title's upscaler
#   scripts\play-viscosity.ps1 -More @{VICTORY_X='1'}   one more switch on top
# Build first: cargo build --locked --profile diag -p victory --features diagnostics
# The log goes to runs\<Name>.log. The first run of a place is slow while shaders are translated.
param(
    [string]$GameDir = $env:VICTORY_GAME_DIR,
    [ValidateSet('720', '810', '1080')][string]$World = '1080',
    [switch]$Fullscreen,
    [string]$Window = '1936x1119',
    [string]$Name = ("viscosity-" + (Get-Date -Format 'MMdd-HHmm')),
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
}
if ($Fullscreen) { $settings.VICTORY_FULLSCREEN = '1' } else { $settings.VICTORY_WINDOW_SIZE = $Window }
$settings.VICTORY_RENDER_SCALE = @{ '720' = '0.667'; '810' = '0.75'; '1080' = '1.0' }[$World]
if ($World -eq '1080') { $settings.VICTORY_SHARPEN = '0.3' }
foreach ($k in $More.Keys) { $settings[$k] = $More[$k] }
$runs = Join-Path $root 'runs'; New-Item -ItemType Directory -Force $runs | Out-Null
$log = Join-Path $runs "$Name.log"
Write-Host "Starting VIscosity (world $World), log $log"
& "$PSScriptRoot\play-nvidia.ps1" -Exe $exe -NoSmoothMotion -Build diag -More $settings 2> $log
