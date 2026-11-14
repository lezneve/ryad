# compares release profiles (docs/measurements.md, "профілі збірки"):
# builds each into its own target dir, then times opening and saving a 10 mib
# log with --trace. synthetic file, isolated data folders.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\profiles.ps1 [-Runs 5]
param([int] $Runs = 5)
. (Join-Path $PSScriptRoot 'common.ps1')
Set-Location $root

$profiles = [ordered]@{
  'default release (o3, no lto, 16 cgu, unwind)' = @{ CARGO_PROFILE_RELEASE_OPT_LEVEL = '3'; CARGO_PROFILE_RELEASE_LTO = 'false'; CARGO_PROFILE_RELEASE_CODEGEN_UNITS = '16'; CARGO_PROFILE_RELEASE_STRIP = 'none'; CARGO_PROFILE_RELEASE_PANIC = 'unwind' }
  'o3 + lto + 1 cgu + strip + abort' = @{ CARGO_PROFILE_RELEASE_OPT_LEVEL = '3' }
  'size s (ryad)' = @{}
  'size z' = @{ CARGO_PROFILE_RELEASE_OPT_LEVEL = 'z' }
}

$file = Join-Path ([IO.Path]::GetTempPath()) 'ryad-profile-10mib.log'
$sw = New-Object IO.StreamWriter($file, $false, (New-Object Text.UTF8Encoding($false)))
for ($i = 0; $sw.BaseStream.Length -lt 10MB; $i++) { $sw.Write(('{0:D7} 2026-11-14 12:00:00 info request done in 12 ms' -f $i) + "`r`n"); if ($i % 1000 -eq 0) { $sw.Flush() } }
$sw.Close()

foreach ($name in $profiles.Keys) {
  $dir = 'target\prof-' + ($name -replace '[^a-z0-9]+', '-').Trim('-')
  $envs = $profiles[$name]
  foreach ($k in $envs.Keys) { Set-Item "env:$k" $envs[$k] }
  cargo build --release --quiet --target-dir $dir 2>&1 | Out-Null
  foreach ($k in $envs.Keys) { Remove-Item "env:$k" }
  $bin = Join-Path $dir 'release\ryad.exe'
  $size = (Get-Item $bin).Length
  $open = @(); $save = @(); $ready = @()
  for ($r = 0; $r -lt $Runs; $r++) {
    $d = New-DataDir prof
    $copy = Join-Path $d 'log.txt'; Copy-Item $file $copy
    $psi = New-Object Diagnostics.ProcessStartInfo $bin
    $psi.Arguments = "--trace --data `"$d`" `"$copy`""
    $psi.UseShellExecute = $false; $psi.RedirectStandardError = $true
    $p = [Diagnostics.Process]::Start($psi)
    $sb = New-Object Text.StringBuilder
    $ev = Register-ObjectEvent -InputObject $p -EventName ErrorDataReceived -MessageData $sb -Action { if ($EventArgs.Data) { [void]$Event.MessageData.AppendLine($EventArgs.Data) } }
    $p.BeginErrorReadLine()
    $deadline = (Get-Date).AddSeconds(30)
    while (-not $sb.ToString().Contains('ready for input') -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 20 }
    $w = [RyadWin]::MainWindow([uint32]$p.Id)
    $e = [RyadWin]::VisibleEditor($w)
    [RyadWin]::Paste($e, 'x')
    [void][RyadWin]::SendMessage($w, 0x0111, [IntPtr]102, [IntPtr]::Zero) # save
    while (-not $sb.ToString().Contains('save:') -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 20 }
    [void][RyadWin]::PostMessage($w, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
    [void]$p.WaitForExit(20000)
    Start-Sleep -Milliseconds 100
    Unregister-Event -SourceIdentifier $ev.Name
    $log = $sb.ToString()
    $open += [double]([regex]::Match($log, 'open: ([\d.]+) ms').Groups[1].Value)
    $ready += [double]([regex]::Match($log, 'ryad\s+([\d.]+)\] ready for input').Groups[1].Value)
    $save += [double]([regex]::Match($log, 'save: \d+ bytes in ([\d.]+) ms').Groups[1].Value)
    Remove-Item -Recurse -Force $d
  }
  $med = { param($v) $s = $v | Sort-Object; $s[[int][math]::Floor($s.Count / 2)] }
  '| {0} | {1:N0} | {2:N0} ({3:N0}–{4:N0}) | {5:N0} ({6:N0}–{7:N0}) | {8:N1} ({9:N1}–{10:N1}) |' -f $name, ($size / 1KB), (& $med $open), ($open | Measure-Object -Minimum).Minimum, ($open | Measure-Object -Maximum).Maximum, (& $med $ready), ($ready | Measure-Object -Minimum).Minimum, ($ready | Measure-Object -Maximum).Maximum, (& $med $save), ($save | Measure-Object -Minimum).Minimum, ($save | Measure-Object -Maximum).Maximum
}
Remove-Item $file
