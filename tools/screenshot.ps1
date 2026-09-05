# Capture a window belonging to a named process.
#
#   powershell -File tools/screenshot.ps1 -Process folio -Out shot.png
#
# Uses PrintWindow with PW_RENDERFULLCONTENT so the capture works even when the
# window is occluded or the session has no interactive desktop — which is the
# usual case when this runs from an agent. Falls back to a screen grab.
#
# Used to look at the app during development. A blank frame means the window
# came up but the webview did not.

param(
    [string]$Process = "folio",
    [string]$Out = "shot.png",
    [int]$WaitSeconds = 0,
    [int]$Index = 0
)

Add-Type -AssemblyName System.Drawing

# Without this, Windows virtualises every coordinate this script reads and
# every pixel it captures, and the numbers quietly stop meaning anything.
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class Dpi{[DllImport("user32.dll")]public static extern bool SetProcessDPIAware();}'
[Dpi]::SetProcessDPIAware() | Out-Null

$code = @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Runtime.InteropServices;
using System.Text;

public class WinShot {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

    /// Top-level windows of a process that actually have area, largest first.
    public static List<IntPtr> RealWindows(uint want) {
        var found = new List<IntPtr>();
        var areas = new List<int>();
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid != want || !IsWindowVisible(h)) return true;
            RECT r; GetWindowRect(h, out r);
            int area = (r.Right - r.Left) * (r.Bottom - r.Top);
            // Skip Tao's 15x15 message-only event target and friends.
            if (area < 40000) return true;
            found.Add(h); areas.Add(area);
            return true;
        }, IntPtr.Zero);
        for (int i = 0; i < found.Count; i++)
            for (int j = i + 1; j < found.Count; j++)
                if (areas[j] > areas[i]) {
                    var th = found[i]; found[i] = found[j]; found[j] = th;
                    var ta = areas[i]; areas[i] = areas[j]; areas[j] = ta;
                }
        return found;
    }

    public static Size SizeOf(IntPtr h) {
        RECT r; GetWindowRect(h, out r);
        return new Size(r.Right - r.Left, r.Bottom - r.Top);
    }

    public static Point OriginOf(IntPtr h) {
        RECT r; GetWindowRect(h, out r);
        return new Point(r.Left, r.Top);
    }

    /// Ask the window to draw itself. PW_RENDERFULLCONTENT (2) is what makes
    /// this work for a WebView2 surface.
    public static Bitmap Print(IntPtr h) {
        var size = SizeOf(h);
        var bmp = new Bitmap(size.Width, size.Height);
        using (var g = Graphics.FromImage(bmp)) {
            IntPtr hdc = g.GetHdc();
            PrintWindow(h, hdc, 2);
            g.ReleaseHdc(hdc);
        }
        return bmp;
    }

    public static bool LooksBlank(Bitmap bmp) {
        // Sample a grid; if every sample is the same colour, nothing rendered.
        Color first = bmp.GetPixel(0, 0);
        for (int y = 0; y < bmp.Height; y += Math.Max(1, bmp.Height / 40))
            for (int x = 0; x < bmp.Width; x += Math.Max(1, bmp.Width / 40))
                if (bmp.GetPixel(x, y) != first) return false;
        return true;
    }
}
'@
Add-Type -TypeDefinition $code -ReferencedAssemblies System.Drawing, System.Windows.Forms

if ($WaitSeconds -gt 0) { Start-Sleep -Seconds $WaitSeconds }

$proc = Get-Process -Name $Process -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) {
    Write-Error "No process named '$Process'."
    exit 1
}

$windows = [WinShot]::RealWindows([uint32]$proc.Id)
if ($windows.Count -eq 0) {
    Write-Error "Process '$Process' has no window with any area."
    exit 1
}
if ($Index -ge $windows.Count) { $Index = 0 }
$handle = $windows[$Index]

[WinShot]::ShowWindow($handle, 9) | Out-Null   # SW_RESTORE
[WinShot]::SetForegroundWindow($handle) | Out-Null
Start-Sleep -Milliseconds 600

$bitmap = [WinShot]::Print($handle)
$blank = [WinShot]::LooksBlank($bitmap)

if ($blank) {
    # PrintWindow can come back empty for a composited surface; try the screen.
    $size = [WinShot]::SizeOf($handle)
    $origin = [WinShot]::OriginOf($handle)
    $fallback = New-Object System.Drawing.Bitmap $size.Width, $size.Height
    $g = [System.Drawing.Graphics]::FromImage($fallback)
    $g.CopyFromScreen($origin.X, $origin.Y, 0, 0, $size)
    $g.Dispose()
    if (-not [WinShot]::LooksBlank($fallback)) {
        $bitmap.Dispose()
        $bitmap = $fallback
        $blank = $false
    } else {
        $fallback.Dispose()
    }
}

$bitmap.Save((Resolve-Path -LiteralPath (Split-Path -Parent $Out)).Path + "\" + (Split-Path -Leaf $Out),
             [System.Drawing.Imaging.ImageFormat]::Png)
$w = $bitmap.Width; $h = $bitmap.Height
$bitmap.Dispose()

Write-Output "$Out  ($w x $h)$(if ($blank) { '  [BLANK - nothing rendered]' })"
