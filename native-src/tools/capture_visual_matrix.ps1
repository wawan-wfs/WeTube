# Run on an interactive, unlocked Windows desktop with sufficient display space.
# Captures actual production pages, NOT a declaration of visual acceptance.
# Use only fixture media/channel IDs and query that you are authorized to access.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$OutDir,
    [Parameter(Mandatory)][ValidatePattern('^[A-Za-z0-9_-]{11}$')][string]$VideoId,
    [Parameter(Mandatory)][ValidatePattern('^[A-Za-z0-9_-]{11}$')][string]$ShortId,
    [Parameter(Mandatory)][ValidatePattern('^UC[A-Za-z0-9_-]{22}$')][string]$ChannelId,
    [Parameter(Mandatory)][string]$SearchQuery,
    [Parameter(Mandatory)][switch]$AuthorizedFixtures,
    [ValidateRange(480,2160)][int]$Height=720,
    [ValidateRange(0,480)][int]$RequiredDpi=0,
    [ValidateRange(1,30)][int]$SettleSeconds=3
)
$ErrorActionPreference='Stop'
if (-not $IsWindows -or -not $AuthorizedFixtures) { throw 'Windows PowerShell 7+ and authorized fixtures required.' }
if (-not [Environment]::UserInteractive) { throw 'Interactive desktop required.' }
$Exe=(Resolve-Path -LiteralPath $Exe).Path
if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) { throw 'Executable required.' }
$OutDir=[IO.Path]::GetFullPath($OutDir)
if (Test-Path -LiteralPath $OutDir) { throw 'Use a new output directory; existing evidence is never overwritten.' }
[void](New-Item -ItemType Directory -Path $OutDir)
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class WeTubeCapture {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left,Top,Right,Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X,Y; }
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
}
'@
$previousContext=[WeTubeCapture]::SetThreadDpiAwarenessContext([IntPtr](-4))
if ($previousContext -eq [IntPtr]::Zero) { throw 'Could not enable per-monitor DPI-aware capture.' }
$pages=@('Home','Watch','Shorts','Search','Channel','Subscriptions','Library','Playlist','Downloads','Settings')
$rows=[System.Collections.Generic.List[object]]::new()
$exeHash=(Get-FileHash -Algorithm SHA256 -LiteralPath $Exe).Hash.ToLowerInvariant()
function Read-State($file) {
    if (Test-Path -LiteralPath $file -PathType Leaf) {
        try { return (Get-Content -LiteralPath $file -Raw | ConvertFrom-Json) } catch { return $null }
    }
    return $null
}
function Wait-State($process,$file,$page,$width,$height,$scale,$focus) {
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    do {
        if ($process.HasExited) { return $null }
        $s=Read-State $file
        if ($null -ne $s -and $s.page -ceq $page -and [Math]::Abs([double]$s.viewport_width-$width) -le 2 -and [Math]::Abs([double]$s.viewport_height-$height) -le 2 -and [Math]::Abs([double]$s.ui_scale-$scale) -lt 0.001) {
            if ($focus -eq 'any' -or ($focus -eq 'root' -and $s.root_focused) -or ($focus -eq 'search' -and $s.search_focused)) { return $s }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    return $null
}
function Assert-Foreground([IntPtr]$h,[int]$processId) {
    [uint32]$owner=0
    [void][WeTubeCapture]::GetWindowThreadProcessId($h,[ref]$owner)
    if ($owner -ne $processId -or -not [WeTubeCapture]::IsWindowVisible($h) -or [WeTubeCapture]::IsIconic($h) -or [WeTubeCapture]::GetForegroundWindow() -ne $h) { throw 'Own visible foreground window could not be verified.' }
}
try {
    foreach ($width in @(720,856,1280,1920)) { foreach ($scale in @(0.75,1.0,1.25,1.5)) { foreach ($page in $pages) {
        $percent=[int]($scale*100)
        $cell="$page-$width-$percent"
        $profile=Join-Path $OutDir ("profile-"+[Guid]::NewGuid().ToString('N'))
        [void](New-Item -ItemType Directory -Path $profile)
        $stateFile=Join-Path $profile 'visual-state.json'
        $target=switch ($page) { 'Watch' {$VideoId}; 'Playlist' {$VideoId}; 'Shorts' {$ShortId}; 'Channel' {$ChannelId}; 'Search' {$SearchQuery}; default {'-'} }
        $row=[ordered]@{page=$page;width=$width;height=$Height;scale=$scale;status='NOT TESTED';focus='NOT TESTED';focus_trace=@();visual_review='NOT REVIEWED';reason='';screenshot=$null;sha256=$null;dpi=$null;pixel_width=$null;pixel_height=$null;state=$null;profile=[IO.Path]::GetFileName($profile)}
        $process=$null
        try {
            $psi=[Diagnostics.ProcessStartInfo]::new($Exe)
            $psi.UseShellExecute=$false
            $psi.WorkingDirectory=[IO.Path]::GetDirectoryName($Exe)
            $psi.Environment['WETUBE_NATIVE_DATA_DIR']=$profile
            foreach ($arg in @('--visual-probe',$page,[string]$width,[string]$Height,$scale.ToString([Globalization.CultureInfo]::InvariantCulture),$target)) { $psi.ArgumentList.Add($arg) }
            $process=[Diagnostics.Process]::Start($psi)
            $s=Wait-State $process $stateFile $page $width $Height $scale 'any'
            if ($null -eq $s) { throw 'Requested route/viewport/scale did not render; screen may be too small.' }
            Start-Sleep -Seconds $SettleSeconds
            $process.Refresh();$h=$process.MainWindowHandle
            if ($h -eq [IntPtr]::Zero) { throw 'No native window handle.' }
            [void][WeTubeCapture]::SetForegroundWindow($h);Start-Sleep -Milliseconds 500
            Assert-Foreground $h $process.Id
            $dpi=[WeTubeCapture]::GetDpiForWindow($h)
            $row.dpi=$dpi
            if ($dpi -eq 0 -or ($RequiredDpi -ne 0 -and $dpi -ne $RequiredDpi)) { throw 'OS DPI did not match requested DPI.' }
            if ([Math]::Abs([double]$s.os_scale-($dpi/96.0)) -gt 0.02) { throw 'GPUI DPI and OS DPI disagree.' }
            # F6 is tested by reading actual GPUI focus state, not by assuming SendKeys succeeds.
            if ($s.root_focused) {
                $row.focus_trace=@($s)
                [System.Windows.Forms.SendKeys]::SendWait('{F6}')
                $focusState=Wait-State $process $stateFile $page $width $Height $scale 'search'
                if ($null -ne $focusState) {
                    $row.focus_trace+=@($focusState)
                    Assert-Foreground $h $process.Id
                    [System.Windows.Forms.SendKeys]::SendWait('{F6}')
                    $focusState=Wait-State $process $stateFile $page $width $Height $scale 'root'
                    if ($null -ne $focusState) { $row.focus_trace+=@($focusState); $row.focus='PASS' } else { $row.focus='FAIL' }
                } else { $row.focus='FAIL' }
            } else { $row.focus='NOT TESTED' }
            $s=Wait-State $process $stateFile $page $width $Height $scale 'any'
            if ($null -eq $s) { throw 'Route or geometry changed during focus test.' }
            Assert-Foreground $h $process.Id
            $rect=[WeTubeCapture+RECT]::new();$point=[WeTubeCapture+POINT]::new()
            if (-not [WeTubeCapture]::GetClientRect($h,[ref]$rect) -or -not [WeTubeCapture]::ClientToScreen($h,[ref]$point)) { throw 'Could not measure client area.' }
            $w=$rect.Right-$rect.Left;$hh=$rect.Bottom-$rect.Top
            if ($w -le 0 -or $hh -le 0 -or [Math]::Abs($w-([double]$s.viewport_width*$dpi/96.0)) -gt 4 -or [Math]::Abs($hh-([double]$s.viewport_height*$dpi/96.0)) -gt 4) { throw 'Measured client pixels disagree with GPUI viewport.' }
            $bounds=[System.Drawing.Rectangle]::new($point.X,$point.Y,$w,$hh)
            if (-not [System.Windows.Forms.Screen]::FromHandle($h).Bounds.Contains($bounds)) { throw 'Client area is clipped by display bounds.' }
            $name="$cell.png";$path=Join-Path $OutDir $name
            $bitmap=[System.Drawing.Bitmap]::new($w,$hh)
            try {
                $graphics=[System.Drawing.Graphics]::FromImage($bitmap)
                try { $graphics.CopyFromScreen($point.X,$point.Y,0,0,$bitmap.Size) } finally { $graphics.Dispose() }
                Assert-Foreground $h $process.Id
                $bitmap.Save($path,[System.Drawing.Imaging.ImageFormat]::Png)
            } finally { $bitmap.Dispose() }
            $row.status='CAPTURED';$row.screenshot=$name;$row.sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant();$row.pixel_width=$w;$row.pixel_height=$hh;$row.state=$s
        } catch { $row.reason=$_.Exception.Message }
        finally {
            if ($null -ne $process) { try { if (-not $process.HasExited) { $process.Kill();$process.WaitForExit() } } finally { $process.Dispose() } }
            $rows.Add([pscustomobject]$row)
            $report=[ordered]@{schema=1;scope='Actual visible production client area; screenshot capture is not visual review or provider acceptance';exe_sha256=$exeHash;required_dpi=$RequiredDpi;cells=@($rows.ToArray())}
            $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $OutDir 'matrix.json') -Encoding utf8
            Write-Host "$cell : $($row.status) / focus=$($row.focus)"
        }
    } } }
} finally { [void][WeTubeCapture]::SetThreadDpiAwarenessContext($previousContext) }
