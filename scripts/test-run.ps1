# One timed run of this tree's build, for measuring: the title is started without a terminal window, its logs go
# to runs\<Name>\out.log and err.log, pictures of its window are taken at 45, 90, 150, 210 and 270 s, and the window
# is closed after -Seconds.
#   scripts\test-run.ps1 -Name try1 -Seconds 300 -Pregrow 24
#   scripts\test-run.ps1 -Name p1 -Seconds 240 -Pregrow 24 -Set "VICTORY_PROFILE=150,165,4,$PWD\runs\p1-profile.txt"
#   python tools\runs\sum.py runs\try1\err.log
# The settings are scripts\play-nvidia.ps1's speed set with scripts\play-miamiwalk.ps1's on top (keep the three in
# step), plus what a hands-off run needs: the log at info, the pipeline book, and Enter pressed through the menus.
# -Set "NAME=value","NAME=value" changes or adds settings for the run.
# -Pregrow <GB> takes that much commit before the start, so that Windows grows the page file before the title needs
# it, and gives it back a GB every two seconds from 15 s on. (On a 32 GB PC the title's process takes 22-24 GB.)
param(
    [string]$Name = "run",
    [int]$Seconds = 300,
    [string]$Exe = 'C:\Games\test-game\game_scarlett_master.exe',
    [string]$Console = "series-s",
    [string]$Data = "$env:LOCALAPPDATA\VIctory\data-save",
    [string[]]$Set = @(),
    [int]$Pregrow = 0,
    [ValidateSet('diag', 'release')][string]$Build = "diag"
)
$root = Split-Path $PSScriptRoot
$victory = Join-Path $root "target\$Build\victory.exe"
if (-not (Test-Path $victory)) { "No $victory. Build it first: cargo build --locked --profile diag -p victory --features diagnostics"; return }
if (-not (Test-Path $Exe)) { "No $Exe."; return }
if (Get-Process victory -ErrorAction SilentlyContinue) { "victory.exe is running already (the title, or victory warm)."; return }
$out = Join-Path $root "runs\$Name"
New-Item -ItemType Directory -Force $out | Out-Null
$settings = [ordered]@{
    VICTORY_GAME_DIR = (Split-Path $Exe); VICTORY_CACHE_DIR = "$env:LOCALAPPDATA\VIctory\cache-nvidia"; VICTORY_DATA_DIR = $Data
    # scripts\play-nvidia.ps1
    VICTORY_TEXTURE_NO_WAIT = "1"; VICTORY_RELAY = "2"; VICTORY_CARD_STEPS = "2"; VICTORY_WIDE_BACK = "1"
    VICTORY_CARD_REHASH_RUNS = "1"; VICTORY_JOIN_COPIES = "1"; VICTORY_INDIRECT_WINDOW = "1"; VICTORY_RELAY_COPY = "1"
    VICTORY_INDIRECT_BATCH = "2"; VICTORY_MAP_THREADS = "1"; VICTORY_TITLE_PRIORITIES = "0"; VICTORY_CARD_AWAKE = "1"
    VICTORY_SHOW_AFTER = "1"; VICTORY_AMBIENT = "0.8"; VICTORY_D3D12_DRED = "light"; VICTORY_SKIP_PIPELINES = "none"
    VICTORY_THIN = "1"; VICTORY_JOIN_PARTS = "8"; VICTORY_UPLOAD_AHEAD = "1"
    VICTORY_AMBIENT_AIR = "1.5"; VICTORY_AMBIENT_MOST = "2"; VICTORY_NAN_SCRUB = "1"
    VICTORY_PIPELINE_WAIT_MS = "10000"; VICTORY_DYNAMIC_SLOTS = "524288"
    VICTORY_OWN_UPSCALER = "1"; VICTORY_BVH_MISS = "1"; VICTORY_COMPOSE = "1"
    # scripts\play-miamiwalk.ps1
    VICTORY_CARD_BUFFERS_MB = "3072"; VICTORY_CARD_BUFFERS_MIN_KB = "64"
    VICTORY_RENDER_SCALE = "1.0"; VICTORY_SHARPEN = "0.3"; VICTORY_BURY_LAG = "24"
    VICTORY_RECHECK_FRAMES = "2,4,8,16,32,64,128,256"; VICTORY_RECHECK_REFILLS = "4"
    VICTORY_DISPLAY_MODE = "2560x1440"; VICTORY_WINDOW_SIZE = "1936x1119"
    # A run nobody sits at
    VICTORY_LOG = "info"; VICTORY_PIPELINE_BOOK = "1"
    VICTORY_KEYS = "8:Enter,12:Enter,16:Enter,20:Enter,24:Enter,28:Enter,32:Enter,36:Enter,40:Enter,45:Enter,50:Enter,55:Enter,60:Enter"
}
# Ray tracing mostly off (the skip list is a diagnostics switch: the diag build).
$rt = Join-Path $PSScriptRoot "rt_off_hashes.txt"
if (Test-Path $rt) { $settings.VICTORY_SKIP_PIPELINES = (Get-Content $rt -Raw).Trim() }
foreach ($pair in $Set) { $k, $v = $pair.Split("=", 2); $settings[$k] = $v }
Get-ChildItem Env:VICTORY_* | ForEach-Object { Remove-Item "Env:$($_.Name)" }
foreach ($k in $settings.Keys) { Set-Item -Path "env:$k" -Value $settings[$k] }
$settings.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" } | Set-Content (Join-Path $out "settings.txt")

Add-Type -ReferencedAssemblies System.Drawing @"
using System; using System.Runtime.InteropServices; using System.Drawing; using System.Text;
public static class Grab {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    public delegate bool EnumProc(IntPtr h, IntPtr p);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr p);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
    [DllImport("user32.dll")] static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
    public static IntPtr Find(uint pid) {
        IntPtr found = IntPtr.Zero; int best = 0;
        EnumWindows((h, p) => { uint q; GetWindowThreadProcessId(h, out q);
            if (q == pid && IsWindowVisible(h)) { RECT r; GetWindowRect(h, out r); int a = (r.R - r.L) * (r.B - r.T); if (a > best) { best = a; found = h; } }
            return true; }, IntPtr.Zero);
        return found;
    }
    public static string Shot(IntPtr h, string path) {
        RECT w, c; GetWindowRect(h, out w); GetClientRect(h, out c);
        var title = new StringBuilder(512); GetWindowText(h, title, 512);
        using (var bmp = new Bitmap(Math.Max(1, w.R - w.L), Math.Max(1, w.B - w.T)))
        using (var g = Graphics.FromImage(bmp)) {
            IntPtr dc = g.GetHdc(); PrintWindow(h, dc, 2); g.ReleaseHdc(dc);
            bmp.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
        return String.Format("window {0}x{1}, client {2}x{3}, title: {4}", w.R - w.L, w.B - w.T, c.R, c.B, title);
    }
    public static void Close(IntPtr h) { PostMessage(h, 0x0010, IntPtr.Zero, IntPtr.Zero); }
}
"@

Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Commit {
    [DllImport("kernel32.dll")] static extern IntPtr VirtualAlloc(IntPtr a, UIntPtr n, uint type, uint protect);
    [DllImport("kernel32.dll")] static extern bool VirtualFree(IntPtr a, UIntPtr n, uint type);
    public static IntPtr Take() { return VirtualAlloc(IntPtr.Zero, (UIntPtr)(1UL << 30), 0x3000, 0x04); }
    public static void Give(IntPtr a) { VirtualFree(a, UIntPtr.Zero, 0x8000); }
}
"@
$held = New-Object System.Collections.Generic.List[IntPtr]
if ($Pregrow -gt 0) {
    $tries = 0
    while ($held.Count -lt $Pregrow -and $tries -lt 600) {
        $a = [Commit]::Take()
        if ($a -eq [IntPtr]::Zero) { $tries++; Start-Sleep -Milliseconds 500 } else { $held.Add($a) }
    }
    $pf = Get-CimInstance Win32_PageFileUsage
    "held {0} GB of commit before the start ({1} failed tries); page file now {2} MB" -f $held.Count, $tries, ($pf.AllocatedBaseSize | Measure-Object -Sum).Sum
}
# Started without a console window of its own: through a hidden command processor that writes
# the two logs. (Start-Process gave the engine, a console program, a terminal window every run.)
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $env:ComSpec
$psi.Arguments = '/d /c ""{0}" run "{1}" --console {2} --execute > "{3}\out.log" 2> "{3}\err.log""' -f $victory, $Exe, $Console, $out
# (victory.toml is looked for in the tree's folder.)
$psi.WorkingDirectory = $root
$psi.UseShellExecute = $false
$psi.CreateNoWindow = $true
$shell = [System.Diagnostics.Process]::Start($psi)
$p = $null
for ($i = 0; $i -lt 150 -and -not $p; $i++) {
    Start-Sleep -Milliseconds 100
    $p = Get-Process victory -ErrorAction SilentlyContinue | Select-Object -First 1
}
if (-not $p) { "the title did not start"; foreach ($a in $held) { [Commit]::Give($a) }; return }
$started = Get-Date
$shots = @(45, 90, 150, 210, 270) | Where-Object { $_ -lt $Seconds }
$n = 0
while (-not $p.HasExited -and ((Get-Date) - $started).TotalSeconds -lt $Seconds) {
    Start-Sleep -Seconds 2
    $t = ((Get-Date) - $started).TotalSeconds
    # From 15 s on, a GB given back every 2 s: the title's own commit rises about that fast.
    if ($t -ge 15 -and $held.Count -gt 0) { [Commit]::Give($held[$held.Count - 1]); $held.RemoveAt($held.Count - 1) }
    if ($n -lt $shots.Count -and $t -ge $shots[$n]) {
        $h = [Grab]::Find([uint32]$p.Id)
        if ($h -ne [IntPtr]::Zero) { "{0:n0} s: {1}" -f $t, [Grab]::Shot($h, (Join-Path $out ("shot-{0}.png" -f $shots[$n]))) }
        $os = Get-CimInstance Win32_OperatingSystem
        $p.Refresh()
        $pf = (Get-CimInstance Win32_PageFileUsage | Measure-Object -Property AllocatedBaseSize -Sum).Sum
        "{0:n0} s: process private {1:n1} GB, working set {2:n1} GB; system commit free {3:n1} GB, RAM free {4:n1} GB; page file {5} MB; still held {6} GB" -f $t, ($p.PrivateMemorySize64 / 1GB), ($p.WorkingSet64 / 1GB), ($os.FreeVirtualMemory / 1MB), ($os.FreePhysicalMemory / 1MB), $pf, $held.Count
        $n++
    }
}
if ($p.HasExited) {
    [void]$shell.WaitForExit(5000)
    "ended by itself after {0:n0} s, exit code {1}" -f ((Get-Date) - $started).TotalSeconds, $(if ($shell.HasExited) { $shell.ExitCode } else { "?" })
} else {
    $h = [Grab]::Find([uint32]$p.Id)
    if ($h -ne [IntPtr]::Zero) { [Grab]::Close($h) }
    if (-not $p.WaitForExit(20000)) { try { $p.Kill() } catch {}; "closed after $Seconds s (had to be ended)" } else { "closed after $Seconds s (window closed)" }
}

foreach ($a in $held) { [Commit]::Give($a) }
