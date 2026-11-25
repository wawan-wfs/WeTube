$ErrorActionPreference = 'Stop'
$root = (Resolve-Path .).Path
$evidence = Join-Path $root 'installer-evidence'
New-Item -ItemType Directory -Force $evidence | Out-Null
$app = Join-Path $env:RUNNER_TEMP ('wetube-install-' + [Guid]::NewGuid().ToString('N'))
$env:WETUBE_NATIVE_DATA_DIR = Join-Path $env:RUNNER_TEMP ('wetube-profile-' + [Guid]::NewGuid().ToString('N'))
$setup = Get-ChildItem native-src/dist/*-setup.exe -File
if (@($setup).Count -ne 1) { throw 'Expected exactly one installer' }
function Run-Wait($file, $arguments, $seconds) {
    $p = Start-Process -FilePath $file -ArgumentList $arguments -PassThru
    if (!$p.WaitForExit($seconds * 1000)) {
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
        throw "Process timeout: $file"
    }
    if ($p.ExitCode -ne 0) { throw "Process failed: $file exit $($p.ExitCode)" }
}
function Quote($s) { return ([char]34 + $s + [char]34) }
try {
    Run-Wait $setup.FullName @('/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',('/DIR=' + (Quote $app)),('/LOG=' + (Quote (Join-Path $evidence 'install.log')))) 600
    $exe = Join-Path $app 'wetube.exe'
    $dll = Join-Path $app 'libmpv-2.dll'
    if (!(Test-Path $exe -PathType Leaf) -or !(Test-Path $dll -PathType Leaf)) { throw 'Installed exe or DLL missing' }
    $sha = (Get-FileHash $dll -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($sha -ne '88504ee5560942a955405e99dfb5971368e4cf851321f36565bf2a667271db1b') { throw 'Installed DLL hash mismatch' }
    $exeSha = (Get-FileHash $exe -Algorithm SHA256).Hash
    $buildSha = (Get-FileHash native-src/target/release/wetube.exe -Algorithm SHA256).Hash
    if ($exeSha -ne $buildSha) { throw 'Installed executable hash mismatch' }
    $vi = (Get-Item -LiteralPath $exe).VersionInfo
    if ($vi.ProductName -ne 'WeTube Desktop by WFS Project' -or $vi.FileDescription -ne 'WeTube Desktop by WFS Project' -or $vi.CompanyName -ne 'WFS Project') { throw 'Installed executable branding metadata mismatch' }
    $si = $setup.VersionInfo
    if ($si.ProductName -ne 'WeTube Desktop by WFS Project' -or $si.CompanyName -ne 'WFS Project') { throw 'Installer branding metadata mismatch' }
    @{status='PASS'; exe_product=$vi.ProductName; exe_description=$vi.FileDescription; exe_company=$vi.CompanyName; setup_product=$si.ProductName; setup_company=$si.CompanyName} | ConvertTo-Json | Set-Content (Join-Path $evidence 'branding.json')
    $fixture = Join-Path $evidence 'local.ppm'
    $header = [Text.Encoding]::ASCII.GetBytes(('P6' + [char]10 + '160 90' + [char]10 + '255' + [char]10))
    $pixels = [byte[]]::new(160 * 90 * 3)
    for ($i=0; $i -lt $pixels.Length; $i+=3) {
        $pixels[$i] = 210; $pixels[$i+1] = 65; $pixels[$i+2] = 125
    }
    [IO.File]::WriteAllBytes($fixture, [byte[]]($header + $pixels))
    $report = Join-Path $evidence 'renderer.json'
    Run-Wait $exe @('--runtime-self-test',(Quote $fixture),(Quote $report)) 45
    $result = Get-Content $report -Raw | ConvertFrom-Json
    if ($result.status -ne 'PASS' -or !$result.file_loaded -or $result.frame_width -le 0 -or $result.frame_height -le 0) { throw 'Native renderer did not pass' }
    $motion = Join-Path $evidence 'motion.avi'
    python tools/motion_fixture.py $motion
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $motionReport = Join-Path $evidence 'motion.json'
    Run-Wait $exe @('--runtime-video-test',(Quote $motion),(Quote $motionReport)) 45
    $moving = Get-Content $motionReport -Raw | ConvertFrom-Json
    if ($moving.status -ne 'PASS' -or !$moving.distinct_frames -or !$moving.seek_confirmed) { throw 'Moving video/seek acceptance failed' }
    $gui = Start-Process -FilePath $exe -PassThru
    try {
        $window = [IntPtr]::Zero
        for ($i=0;$i -lt 30;$i++) {
            Start-Sleep -Seconds 1
            $gui.Refresh()
            if ($gui.HasExited) { throw "GUI exited: $($gui.ExitCode)" }
            $window = $gui.MainWindowHandle
            if ($window -ne [IntPtr]::Zero) { break }
        }
        if ($window -eq [IntPtr]::Zero) { throw 'No native window within 30 seconds' }
        if ($gui.MainWindowTitle -notlike '*WeTube Desktop by WFS Project*') { throw 'Native window title branding mismatch' }
        @{status='PASS'; window_handle=$window.ToInt64(); title=$gui.MainWindowTitle; scope='native window created; no interaction or YouTube playback verified'} | ConvertTo-Json | Set-Content (Join-Path $evidence 'gui-startup.json')
    } finally {
        if (!$gui.HasExited) {
            $null = $gui.CloseMainWindow()
            if (!$gui.WaitForExit(5000)) { Stop-Process -Id $gui.Id -Force }
        }
    }
    @{status='PASS'; installed_exe_sha256=$exeSha; installed_dll_sha256=$sha; installer_sha256=(Get-FileHash $setup.FullName -Algorithm SHA256).Hash} | ConvertTo-Json | Set-Content (Join-Path $evidence 'install.json')
} finally {
    $uninstaller = Join-Path $app 'unins000.exe'
    if (Test-Path $uninstaller -PathType Leaf) {
        Run-Wait $uninstaller @('/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',('/LOG=' + (Quote (Join-Path $evidence 'uninstall.log')))) 120
        if ((Test-Path (Join-Path $app 'wetube.exe')) -or (Test-Path (Join-Path $app 'libmpv-2.dll'))) { throw 'Uninstall left app binaries' }
        @{status='PASS'; app_binaries_removed=$true} | ConvertTo-Json | Set-Content (Join-Path $evidence 'uninstall.json')
    }
    if (Test-Path $env:WETUBE_NATIVE_DATA_DIR) { Remove-Item -LiteralPath $env:WETUBE_NATIVE_DATA_DIR -Recurse -Force }
}
