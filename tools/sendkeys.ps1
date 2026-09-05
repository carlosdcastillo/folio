# Send keystrokes to a process's main window, so the app can be driven the way
# a user drives it: with its own keyboard shortcuts.
#
#   powershell -File tools/sendkeys.ps1 -Process folio -Keys "^r"
#
# SendKeys syntax: ^ = Ctrl, % = Alt, + = Shift.

param(
    [string]$Process = "folio",
    [Parameter(Mandatory = $true)][string]$Keys,
    [int]$DelayMs = 900
)

Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public class Focus {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern IntPtr SetActiveWindow(IntPtr h);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
    public static IntPtr MainWindow(uint want) {
        IntPtr best = IntPtr.Zero; int bestArea = 0;
        EnumWindows((h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (pid != want || !IsWindowVisible(h)) return true;
            RECT r; GetWindowRect(h, out r);
            int area = (r.Right - r.Left) * (r.Bottom - r.Top);
            if (area > bestArea) { bestArea = area; best = h; }
            return true;
        }, IntPtr.Zero);
        return best;
    }
}
'@

$proc = Get-Process -Name $Process -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) { Write-Error "No process named '$Process'."; exit 1 }

$handle = [Focus]::MainWindow([uint32]$proc.Id)
if ($handle -eq [IntPtr]::Zero) { Write-Error "No window to send keys to."; exit 1 }

[Focus]::ShowWindow($handle, 9) | Out-Null
[Focus]::SetForegroundWindow($handle) | Out-Null
[Focus]::SetActiveWindow($handle) | Out-Null
Start-Sleep -Milliseconds 400

[System.Windows.Forms.SendKeys]::SendWait($Keys)
Start-Sleep -Milliseconds $DelayMs
Write-Output "sent: $Keys"
