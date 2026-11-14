# installer, update and uninstall in a sandbox (RYAD_TEST_SANDBOX): files go
# to a temp folder, registry keys under HKCU\Software\ryad-test. the real
# install, the real "open with" entries and the real drafts are not touched.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\install.ps1
. (Join-Path $PSScriptRoot 'common.ps1')

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class RyadTd {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  public static IntPtr Find(uint pid) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      var c = new StringBuilder(64); GetClassName(h, c, 64);
      if (p == pid && c.ToString() == "#32770" && IsWindowVisible(h)) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
'@

$script:pass = 0; $script:fail = 0
function Check([string] $name, [bool] $ok, [string] $detail = '') {
  if ($ok) { $script:pass++; Write-Output "  ok   $name" } else { $script:fail++; Write-Output "  FAIL $name $detail" }
}
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public static class Inst {
  [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr d, int id);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
}
'@
# the installer window of process $p: optionally flips the switches (11: start
# ryad now, 10: notepad), then presses its main button (install / remove /
# close) with BM_CLICK, which returns once the work is done.
function Press($p, [int] $button, [switch] $uncheck, [switch] $notepad) {
  $deadline = (Get-Date).AddSeconds(15)
  do { Start-Sleep -Milliseconds 100; $p.Refresh(); $w = $p.MainWindowHandle } while ($w -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline -and -not $p.HasExited)
  if ($w -eq [IntPtr]::Zero) { return $false }
  Start-Sleep -Milliseconds 200
  if ($uncheck) { [void][Inst]::SendMessage([Inst]::GetDlgItem($w, 11), 0x00F5, [IntPtr]0, [IntPtr]0) }
  if ($notepad) { [void][Inst]::SendMessage([Inst]::GetDlgItem($w, 10), 0x00F5, [IntPtr]0, [IntPtr]0) }
  $ok = [Inst]::GetDlgItem($w, 1)
  if ($ok -eq [IntPtr]::Zero) { return $false }
  [void][Inst]::SendMessage($ok, 0x00F5, [IntPtr]0, [IntPtr]0)  # BM_CLICK
  Start-Sleep -Milliseconds 200
  $true
}
function Run-Exe([string] $file, [string[]] $arguments, [hashtable] $env) {
  $psi = New-Object Diagnostics.ProcessStartInfo $file
  $psi.Arguments = $arguments -join ' '
  $psi.UseShellExecute = $false
  foreach ($k in $env.Keys) { $psi.EnvironmentVariables[$k] = $env[$k] }
  [Diagnostics.Process]::Start($psi)
}
function RegHas([string] $path, [string] $name = '') {
  $k = Get-Item -LiteralPath "HKCU:\Software\ryad-test\$path" -ErrorAction SilentlyContinue
  if (-not $k) { return $false }
  if ($name) { return $k.GetValueNames() -contains $name }
  $true
}

$box = New-DataDir sandbox
$data = Join-Path $box 'data'
$envs = @{ RYAD_TEST_SANDBOX = $box; RYAD_DATA = $data }
$setup = Join-Path $box 'ryad-0.1.0-setup.exe'
Copy-Item $exe $setup
$installed = Join-Path $box 'Programs\ryad\ryad.exe'

Write-Output 'install'
$p = Run-Exe $setup @() $envs
Check 'install window, install pressed (notepad switch on, no start)' (Press $p 101 -uncheck -notepad)
Check 'done page closed' (Press $p 101)
Check 'installer exits 0' ($p.WaitForExit(15000) -and $p.ExitCode -eq 0)
Check 'exe in place' (Test-Path $installed)
Check 'start menu entry' (Test-Path (Join-Path $box 'StartMenu\ryad.lnk'))
Check 'apps entry with version' ((Get-ItemProperty 'HKCU:\Software\ryad-test\Microsoft\Windows\CurrentVersion\Uninstall\ryad' -ErrorAction SilentlyContinue).DisplayVersion -eq '0.1.0')
Check 'progid with open command' ((Get-ItemProperty 'HKCU:\Software\ryad-test\Classes\ryad.text\shell\open\command' -ErrorAction SilentlyContinue).'(default)' -eq "`"$installed`" `"%1`"")
$exts = '.txt', '.log', '.md', '.ini', '.cfg', '.json', '.csv', '.xml'
Check 'open with entries for every type' (@($exts | Where-Object { RegHas "Classes\$_\OpenWithProgids" 'ryad.text' }).Count -eq 8)
Check 'supported types of the application' (@($exts | Where-Object { RegHas 'Classes\Applications\ryad.exe\SupportedTypes' $_ }).Count -eq 8)
Check 'registered application' (RegHas 'RegisteredApplications' 'ryad')
Check 'notepad replaced: marked (the sandbox never touches the real package)' (RegHas 'ryad\Setup' 'notepad_removed')
Check 'no default taken over' (-not (RegHas 'Classes\.txt' '(default)') -and -not ((Get-Item 'HKCU:\Software\ryad-test\Classes\.txt' -ErrorAction SilentlyContinue).GetValue('')))

Write-Output 'the installed ryad keeps a draft; an update closes it first'
$r = Run-Exe $installed @('--data', "`"$data`"") @{}
$deadline = (Get-Date).AddSeconds(10)
do { Start-Sleep -Milliseconds 100; $w = [RyadWin]::MainWindow([uint32]$r.Id) } while ($w -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
do { Start-Sleep -Milliseconds 50; $e = [RyadWin]::VisibleEditor($w) } while ($e -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
[RyadWin]::Paste($e, 'чернетка до оновлення')
$p = Run-Exe $setup @() $envs
Check 'update window' (Press $p 101 -uncheck)
Check 'update done page' (Press $p 101)
[void]$p.WaitForExit(15000)
Check 'running ryad was closed' ($r.HasExited -and $r.ExitCode -eq 0)
$r = Run-Exe $installed @('--data', "`"$data`"") @{}
$deadline = (Get-Date).AddSeconds(10)
do { Start-Sleep -Milliseconds 100; $w = [RyadWin]::MainWindow([uint32]$r.Id) } while ($w -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
do { Start-Sleep -Milliseconds 50; $e = [RyadWin]::VisibleEditor($w) } while ($e -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
Check 'draft back after the update' ([RyadWin]::Text($e) -eq 'чернетка до оновлення')
[void][RyadWin]::PostMessage($w, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero); [void]$r.WaitForExit(10000)

Write-Output 'uninstall'
$p = Run-Exe $installed @('--uninstall') $envs
Check 'remove window' (Press $p 101)
Check 'removed page closed' (Press $p 101)
[void]$p.WaitForExit(15000)
Start-Sleep -Seconds 3
Check 'program folder removed' (-not (Test-Path (Join-Path $box 'Programs\ryad')))
Check 'start menu entry removed' (-not (Test-Path (Join-Path $box 'StartMenu\ryad.lnk')))
Check 'apps entry removed' (-not (RegHas 'Microsoft\Windows\CurrentVersion\Uninstall\ryad'))
Check 'open with entries removed' (@($exts | Where-Object { RegHas "Classes\$_\OpenWithProgids" 'ryad.text' }).Count -eq 0 -and -not (RegHas 'Classes\ryad.text') -and -not (RegHas 'Classes\Applications\ryad.exe') -and -not (RegHas 'RegisteredApplications' 'ryad'))
Check 'notepad mark cleared on uninstall' (-not (RegHas 'ryad\Setup' 'notepad_removed'))
Check 'drafts kept' (Test-Path (Join-Path $data 'session\session.ini'))

Write-Output 'reinstall finds the session'
$p = Run-Exe $setup @() $envs
[void](Press $p 101 -uncheck); [void](Press $p 101); [void]$p.WaitForExit(15000)
$r = Run-Exe $installed @('--data', "`"$data`"") @{}
$deadline = (Get-Date).AddSeconds(10)
do { Start-Sleep -Milliseconds 100; $w = [RyadWin]::MainWindow([uint32]$r.Id) } while ($w -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
do { Start-Sleep -Milliseconds 50; $e = [RyadWin]::VisibleEditor($w) } while ($e -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
Check 'draft back after uninstall + install' ([RyadWin]::Text($e) -eq 'чернетка до оновлення')
[void][RyadWin]::PostMessage($w, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero); [void]$r.WaitForExit(10000)
$p = Run-Exe $installed @('--uninstall') $envs
[void](Press $p 101); [void](Press $p 101); [void]$p.WaitForExit(15000)
Start-Sleep -Seconds 2
Remove-Item -Recurse -Force 'HKCU:\Software\ryad-test' -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force $box -ErrorAction SilentlyContinue

Write-Output "passed $script:pass, failed $script:fail"
if ($script:fail) { exit 1 }
