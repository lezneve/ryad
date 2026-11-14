# baseline measurements of the release build (docs/measurements.md).
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\measure.ps1 [-Runs 10] [-Out research\measure]
# synthetic files only, isolated data folders, no input emulation.
param([int] $Runs = 10, [string] $Out = '')
. (Join-Path $PSScriptRoot 'common.ps1')

function Start-Traced([string] $data, [string[]] $files = @(), [hashtable] $env = @{}) {
  $log = Join-Path $data ('trace-' + [guid]::NewGuid().ToString('n').Substring(0, 6) + '.txt')
  $args = @('--trace', '--data', "`"$data`"") + ($files | ForEach-Object { "`"$_`"" })
  $psi = New-Object Diagnostics.ProcessStartInfo $exe
  $psi.Arguments = $args -join ' '
  $psi.UseShellExecute = $false
  $psi.RedirectStandardError = $true
  foreach ($k in $env.Keys) { $psi.EnvironmentVariables[$k] = $env[$k] }
  $p = [Diagnostics.Process]::Start($psi)
  $sb = New-Object Text.StringBuilder
  $job = Register-ObjectEvent -InputObject $p -EventName ErrorDataReceived -MessageData $sb -Action { if ($EventArgs.Data) { [void]$Event.MessageData.AppendLine($EventArgs.Data) } }
  $p.BeginErrorReadLine()
  $deadline = (Get-Date).AddSeconds(120)
  do { Start-Sleep -Milliseconds 20; $h = [RyadWin]::MainWindow([uint32]$p.Id) } while ($h -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
  while (-not $sb.ToString().Contains('ready for input') -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 20 }
  [pscustomobject]@{ Process = $p; Window = $h; Log = $sb; Job = $job }
}

function Stop-Traced($r) {
  [void][RyadWin]::PostMessage($r.Window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
  [void]$r.Process.WaitForExit(60000)
  Start-Sleep -Milliseconds 100
  Unregister-Event -SourceIdentifier $r.Job.Name
  $r.Log.ToString()
}

function Num([string] $log, [string] $pattern) {
  $m = [regex]::Match($log, $pattern)
  if ($m.Success) { [double]$m.Groups[1].Value } else { [double]::NaN }
}

function Stats([double[]] $v) {
  $s = $v | Where-Object { -not [double]::IsNaN($_) } | Sort-Object
  if (-not $s) { return 'n/a' }
  $n = @($s).Count
  $med = if ($n % 2) { $s[[int][math]::Floor($n / 2)] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 }
  '{0:N1} (min {1:N1}, max {2:N1}, n={3})' -f $med, $s[0], $s[-1], $n
}

$results = [ordered]@{}

# 1. empty start: process start -> window ready for input
$ready = @(); $priv = @(); $ws = @()
for ($i = 0; $i -lt $Runs; $i++) {
  $d = New-DataDir measure
  $r = Start-Traced $d
  $ready += Num $r.Log.ToString() 'ryad\s+([\d.]+)\] ready for input'
  Start-Sleep -Milliseconds 1500
  $p = Get-Process -Id $r.Process.Id
  $priv += $p.PrivateMemorySize64 / 1MB; $ws += $p.WorkingSet64 / 1MB
  [void](Stop-Traced $r)
  Remove-Item -Recurse -Force $d
}
$results['empty start, ready ms'] = Stats $ready
$results['empty start, private MiB after 1.5 s'] = Stats $priv
$results['empty start, working set MiB after 1.5 s'] = Stats $ws

# 2. idle cpu over 10 s and memory after 10 s
$d = New-DataDir measure
$r = Start-Traced $d
Start-Sleep -Seconds 2
$p = Get-Process -Id $r.Process.Id
$c0 = $p.TotalProcessorTime.TotalMilliseconds
Start-Sleep -Seconds 10
$p.Refresh()
$results['idle cpu ms over 10 s'] = '{0:N0}' -f ($p.TotalProcessorTime.TotalMilliseconds - $c0)
$results['idle private MiB / working set MiB'] = '{0:N1} / {1:N1}' -f ($p.PrivateMemorySize64 / 1MB), ($p.WorkingSet64 / 1MB)
$results['idle threads'] = $p.Threads.Count
[void](Stop-Traced $r)
Remove-Item -Recurse -Force $d

# 3. opening files from the command line (cold for ryad, warm file cache)
$files = New-DataDir files
$gen = @(
  @{ name = 'log-1mib.txt'; mib = 1; kind = 'lines' }, @{ name = 'log-10mib.txt'; mib = 10; kind = 'lines' }, @{ name = 'log-50mib.txt'; mib = 50; kind = 'lines' },
  @{ name = 'line-10mib.txt'; mib = 10; kind = 'line' }, @{ name = 'mixed-1mib.txt'; mib = 1; kind = 'mixed' }
)
foreach ($g in $gen) {
  $path = Join-Path $files $g.name
  $sw = New-Object IO.StreamWriter($path, $false, (New-Object Text.UTF8Encoding($false)))
  $target = $g.mib * 1MB
  $i = 0; $written = 0
  while ($written -lt $target) {
    switch ($g.kind) {
      'lines' { $s = ('{0:D7} 2026-11-14 12:00:00 info request done in 12 ms' -f $i) + "`r`n" }
      'line' { $s = 'abcdefghij word ' }
      'mixed' { $s = ('рядок {0} з latin словами і кирилицею, mixed text' -f $i) + "`r`n" }
    }
    $sw.Write($s); $written += [Text.Encoding]::UTF8.GetByteCount($s); $i++
  }
  $sw.Close()
}
foreach ($g in $gen) {
  $path = Join-Path $files $g.name
  $open = @(); $ready = @(); $priv = @()
  $n = if ($g.mib -ge 50) { [math]::Min(3, $Runs) } else { [math]::Min(5, $Runs) }
  for ($i = 0; $i -lt $n; $i++) {
    $d = New-DataDir measure
    $r = Start-Traced $d @($path)
    $log = $r.Log.ToString()
    $open += Num $log 'open: ([\d.]+) ms'
    $ready += Num $log 'ryad\s+([\d.]+)\] ready for input'
    Start-Sleep -Milliseconds 500
    $priv += (Get-Process -Id $r.Process.Id).PrivateMemorySize64 / 1MB
    [void](Stop-Traced $r)
    Remove-Item -Recurse -Force $d
  }
  $results["open $($g.name): open ms"] = Stats $open
  $results["open $($g.name): ready ms"] = Stats $ready
  $results["open $($g.name): private MiB"] = Stats $priv
}
# 4. input latency: a WM_CHAR sent to the text field returns once ryad has
# handled it (the control, EN_CHANGE, the dot, the status line)
function Latency($r, [int] $n) {
  $e = [RyadWin]::VisibleEditor($r.Window)
  $sw = New-Object Diagnostics.Stopwatch
  $t = @()
  for ($i = 0; $i -lt $n; $i++) {
    $sw.Restart(); [void][RyadWin]::SendMessage($e, 0x0102, [IntPtr][int][char]'ї', [IntPtr]1); $sw.Stop()
    $t += $sw.Elapsed.TotalMilliseconds
    Start-Sleep -Milliseconds 20
  }
  $t
}
$d = New-DataDir measure
$r = Start-Traced $d
$results['typing latency ms, new draft (200 chars)'] = Stats (Latency $r 200)
[void](Stop-Traced $r)
Remove-Item -Recurse -Force $d
$d = New-DataDir measure
$r = Start-Traced $d @((Join-Path $files 'log-10mib.txt'))
$results['typing latency ms, 10 mib log (200 chars)'] = Stats (Latency $r 200)
# typing goes on for more than 5 s: commits with a 10 mib snapshot happen meanwhile
$e = [RyadWin]::VisibleEditor($r.Window)
$t = @(); $sw = New-Object Diagnostics.Stopwatch
$end = (Get-Date).AddSeconds(7)
while ((Get-Date) -lt $end) { $sw.Restart(); [void][RyadWin]::SendMessage($e, 0x0102, [IntPtr][int][char]'a', [IntPtr]1); $sw.Stop(); $t += $sw.Elapsed.TotalMilliseconds; Start-Sleep -Milliseconds 30 }
$log = Stop-Traced $r
$results['typing latency ms, 10 mib log, 7 s non-stop (max = a commit)'] = Stats $t
$q = [regex]::Matches($log, 'queued, 1 snapshot\(s\), ([\d.]+) ms on the ui thread') | ForEach-Object { [double]$_.Groups[1].Value }
$results['ui thread per commit with a 10 mib snapshot, ms'] = Stats $q
Remove-Item -Recurse -Force $d

# 5. restore: 2 files of 1 mib (one changed) + 3 drafts
$d = New-DataDir measure
$r = Start-Traced $d @((Join-Path $files 'log-1mib.txt'), (Join-Path $files 'mixed-1mib.txt'))
[RyadWin]::Paste([RyadWin]::VisibleEditor($r.Window), 'changed ')
foreach ($k in 1..3) { [void][RyadWin]::SendMessage($r.Window, 0x0111, [IntPtr]100, [IntPtr]::Zero); [RyadWin]::Paste([RyadWin]::VisibleEditor($r.Window), "draft $k`r`nline two") }
[void](Stop-Traced $r)
$ready = @(); $rest = @()
for ($i = 0; $i -lt 5; $i++) {
  $r = Start-Traced $d
  $log = $r.Log.ToString()
  $ready += Num $log 'ryad\s+([\d.]+)\] ready for input'
  $rest += Num $log 'restore: 5 tab\(s\).*?([\d.]+) ms'
  [void](Stop-Traced $r)
}
$results['restore 5 tabs (2 x 1 mib, one changed, 3 drafts): restore ms'] = Stats $rest
$results['restore 5 tabs: ready ms'] = Stats $ready
Remove-Item -Recurse -Force $d

# 6. memory after closing large tabs (3 x 10 mib open, then closed)
$d = New-DataDir measure
$big = Join-Path $files 'log-10mib.txt'
$c1 = Join-Path $files 'b1.txt'; $c2 = Join-Path $files 'b2.txt'; Copy-Item $big $c1; Copy-Item $big $c2
$r = Start-Traced $d @($big, $c1, $c2)
Start-Sleep -Milliseconds 800
$p = Get-Process -Id $r.Process.Id
$open3 = $p.PrivateMemorySize64 / 1MB
foreach ($k in 1..3) { [void][RyadWin]::SendMessage($r.Window, 0x0111, [IntPtr]104, [IntPtr]::Zero) }
Start-Sleep -Milliseconds 800
$p.Refresh()
$results['private MiB: 3 x 10 mib open -> all closed'] = '{0:N1} -> {1:N1}' -f $open3, ($p.PrivateMemorySize64 / 1MB)
$results['handles after closing'] = $p.HandleCount
[void](Stop-Traced $r)
Remove-Item -Recurse -Force $d

Remove-Item -Recurse -Force $files

$results.GetEnumerator() | ForEach-Object { '| {0} | {1} |' -f $_.Key, $_.Value }
if ($Out) { $results | ConvertTo-Json | Set-Content -Encoding utf8 $Out }
