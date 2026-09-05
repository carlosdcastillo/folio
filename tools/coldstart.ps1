# Measure cold start: process launch to a window with area on screen.
#
#   powershell -File tools/coldstart.ps1 -Exe target/release/folio.exe -Store .demo/store
#
# Polls in-process. Spawning a PowerShell per poll costs more than the thing
# being measured, so a naive loop reports the harness, not the app.

param(
    [string]$Exe = "target/release/folio.exe",
    [string]$Store = ".demo/store",
    [int]$Runs = 5
)

Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class Dpi2{[DllImport("user32.dll")]public static extern bool SetProcessDPIAware();}'
[Dpi2]::SetProcessDPIAware() | Out-Null

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public class Probe {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public static bool HasWindow(uint want) {
        bool found = false;
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid != want || !IsWindowVisible(h)) return true;
            RECT r; GetWindowRect(h, out r);
            if ((r.Right - r.Left) * (r.Bottom - r.Top) > 40000) { found = true; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }
}
'@

$exePath = (Resolve-Path $Exe).Path
$storePath = (Resolve-Path $Store).Path
$env:FOLIO_STORE = $storePath

$results = @()
for ($i = 1; $i -le $Runs; $i++) {
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $exePath -PassThru -WindowStyle Normal

    $shown = $false
    while ($watch.ElapsedMilliseconds -lt 15000) {
        if ([Probe]::HasWindow([uint32]$proc.Id)) { $shown = $true; break }
        Start-Sleep -Milliseconds 5
    }
    $watch.Stop()

    if ($shown) {
        $ms = $watch.ElapsedMilliseconds
        $results += $ms
        Write-Output ("run {0}: {1} ms" -f $i, $ms)
    } else {
        Write-Output ("run {0}: no window within 15s" -f $i)
    }

    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 800
}

if ($results.Count -gt 0) {
    $stats = $results | Measure-Object -Minimum -Maximum -Average
    Write-Output ""
    Write-Output ("min {0} ms   median {1} ms   max {2} ms" -f `
        $stats.Minimum, ($results | Sort-Object)[[int]($results.Count / 2)], $stats.Maximum)
    Write-Output ("budget: 500 ms")
}
