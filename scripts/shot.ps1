# Screenshot of the VIctory game window to E:\work\runs\shots\<name>.png (window content via PrintWindow,
# falling back to a screen copy after bringing it to the front). Prints the file and the window title.
param([string]$Name = ("shot-" + (Get-Date -Format 'MMdd-HHmmss')), [switch]$Front)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class W {
  [StructLayout(LayoutKind.Sequential)] public struct R { public int L, T, Ri, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out R r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
}
"@
$p = Get-Process victory -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $p) { Write-Host 'No game window.'; exit 1 }
$h = $p.MainWindowHandle
$dir = 'E:\work\runs\shots'; New-Item -ItemType Directory -Force $dir | Out-Null
$file = Join-Path $dir "$Name.png"
$r = New-Object W+R; [void][W]::GetWindowRect($h, [ref]$r)
$w = $r.Ri - $r.L; $hh = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$ok = $false
if (-not $Front) {
  $dc = $g.GetHdc(); $ok = [W]::PrintWindow($h, $dc, 2); $g.ReleaseHdc($dc)
  # a black picture means the swap chain was not captured: try the screen instead
  if ($ok) { $s = 0; for ($i = 1; $i -lt 20; $i++) { $c = $bmp.GetPixel([int]($w * $i / 20), [int]($hh / 2)); $s += $c.R + $c.G + $c.B }; if ($s -lt 30) { $ok = $false } }
}
if (-not $ok) {
  [void][W]::ShowWindow($h, 9); [void][W]::SetForegroundWindow($h); Start-Sleep -Milliseconds 700
  [void][W]::GetWindowRect($h, [ref]$r)
  $g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
}
$g.Dispose()
# half size keeps the file small enough to read
$half = New-Object System.Drawing.Bitmap $bmp, ([int]($w / 2)), ([int]($hh / 2))
$half.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose(); $half.Dispose()
"$file  |  $($p.MainWindowTitle)"
