# Starts the title with the settings measured on this PC on 2026-11-26
# (Ryzen 7 7700, RTX 5070 12 GB, 32 GB RAM, 2560x1440 display):
#   scripts\play-miamiwalk.ps1                      the save in %LOCALAPPDATA%\VIctory\data-save
#   scripts\play-miamiwalk.ps1 -Fullscreen          borderless over the whole monitor
#   scripts\play-miamiwalk.ps1 -Window 2560x1440    another window size (frame included)
# What differs from play-nvidia.ps1's own set, and why:
# - VICTORY_CARD_BUFFERS_MB=3072 with VICTORY_CARD_BUFFERS_MIN_KB=64: every copy of a title buffer in the card's
#   memory. With 512 MB of them there the card read the rest across the bus: 3.5 fps against 13-16.
# - The world is drawn at 1920x1080, the size it is shown at (-World 1080, the default; -World 720 or 810 draws it
#   smaller and lets the title's upscaler stretch it). Above 720 the title ran out of two things it sizes by the
#   console and stopped at a breakpoint a few seconds in: its graphics pool (1,277 MB on Series S) and the room for
#   the render targets of a frame (297 MB). The loader gives both their Series X sizes, 2,199 and 710 MB, for those
#   scales (victory-loader hooks.rs; VICTORY_GRAPHICS_POOL_MB sets the pool, 0 leaves both). First run at 1080:
#   nv32, 2026-11-27, 17.8 fps where 720 gave 13-17, the card 72 % busy and 9.0 of 10.8 GB of its memory in use.
# - VICTORY_SHARPEN=0.3: the game's picture lightly sharpened before it is shown (compose.rs); 0.6 at -World 720.
#   (The setting's range is narrow: 0.3 and 0.6 looked the same to the eye.) VICTORY_SMOOTH=1 smooths edges instead
#   and made the picture fuzzy. -More @{VICTORY_SHARPEN='0'} shows the title's picture untouched.
# - VICTORY_RECHECK_FRAMES and VICTORY_RECHECK_REFILLS: textures the title goes on filling after their upload
#   (small black patches) are looked at again sooner, and up to 4 a frame of the long-uploaded ones found changed
#   are uploaded again (draw.rs; 2 by default).
# - VICTORY_BURY_LAG=24: a host list that has run is reset 24 submissions later, not 64. Its descriptor tables go
#   back with it, and at 64 a heavy daytime street held six frames' worth of them: the pool of 500,000 slots ran
#   out, 3,400 tables a frame were refused and their draws dropped (missing heads, blinking). The lag is there
#   against device losses: if the device is lost with this, raise it again.
# - VICTORY_WINDOW_SIZE: 1936x1119 is a picture area of exactly 1920x1080, the game's output, shown 1:1.
param(
    [string]$Exe = 'C:\Games\test-game\game_scarlett_master.exe',
    [string]$Window = '1936x1119',
    [switch]$Fullscreen,
    [ValidateSet('720', '810', '1080')][string]$World = '1080',
    [ValidateSet('diag', 'release')][string]$Build = 'diag',
    [hashtable]$More = @{}
)
$env:VICTORY_CACHE_DIR = "$env:LOCALAPPDATA\VIctory\cache-nvidia"
$env:VICTORY_DATA_DIR = "$env:LOCALAPPDATA\VIctory\data-save"
$env:VICTORY_GAME_DIR = Split-Path $Exe
$settings = @{
    VICTORY_CARD_BUFFERS_MB = '3072'; VICTORY_CARD_BUFFERS_MIN_KB = '64'
    VICTORY_RENDER_SCALE = '0.667'; VICTORY_SHARPEN = '0.6'; VICTORY_BURY_LAG = '24'
    VICTORY_RECHECK_FRAMES = '2,4,8,16,32,64,128,256'; VICTORY_RECHECK_REFILLS = '4'
    VICTORY_DISPLAY_MODE = '2560x1440'
}
if ($Fullscreen) { $settings.VICTORY_FULLSCREEN = '1' } else { $settings.VICTORY_WINDOW_SIZE = $Window }
$settings.VICTORY_RENDER_SCALE = @{ '720' = '0.667'; '810' = '0.75'; '1080' = '1.0' }[$World]
if ($World -eq '1080') { $settings.VICTORY_SHARPEN = '0.3' }
foreach ($k in $More.Keys) { $settings[$k] = $More[$k] }
& "$PSScriptRoot\play-nvidia.ps1" -Exe $Exe -NoSmoothMotion -Build $Build -More $settings
