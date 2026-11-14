# scripted checks of drafts, the session and file safety with the release
# exe. every scenario uses its own data folder and synthetic files.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\session.ps1 [-Only s3]
param([string] $Only = '')
. (Join-Path $PSScriptRoot 'common.ps1')

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class RyadDlg {
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
function Want([string] $s) { -not $Only -or $Only.Split(',') -contains $s }

function Post-Command($r, [int] $id) { [void][RyadWin]::PostMessage($r.Window, 0x0111, [IntPtr]$id, [IntPtr]::Zero) }
# waits for a task dialog of ryad and presses one of its buttons (TDM_CLICK_BUTTON)
function Click-Dialog($r, [int] $button, [int] $timeoutMs = 5000) {
  $deadline = (Get-Date).AddMilliseconds($timeoutMs)
  do { Start-Sleep -Milliseconds 50; $d = [RyadDlg]::Find([uint32]$r.Process.Id) } while ($d -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
  if ($d -eq [IntPtr]::Zero) { return $false }
  Start-Sleep -Milliseconds 100
  [void][RyadDlg]::SendMessage($d, 0x466, [IntPtr]$button, [IntPtr]::Zero)
  Start-Sleep -Milliseconds 150
  $true
}
function Editor($r) { [RyadWin]::VisibleEditor($r.Window) }
function Wait-Commit { Start-Sleep -Milliseconds 1200 }
function Index([string] $d) { Get-Content -Raw -Encoding utf8 (Join-Path $d 'session\session.ini') }
function Tabs($r) { ([RyadWin]::Editors($r.Window)).Count }
$utf8 = New-Object Text.UTF8Encoding($false)
function Write-File([string] $p, [string] $s) { [IO.File]::WriteAllText($p, $s, $utf8) }
function Read-Bytes([string] $p) { [IO.File]::ReadAllBytes($p) }
function Same-Bytes($a, $b) { if ($a.Length -ne $b.Length) { return $false }; for ($i = 0; $i -lt $a.Length; $i++) { if ($a[$i] -ne $b[$i]) { return $false } }; $true }

$draft = "чернетка 😀 з емодзі`r`nдругий рядок ґ є ї і`r`n"
$draftCtl = $draft.Replace("`r`n", "`r")

if (Want 's1') {
  Write-Output 's1 draft survives a full exit: text, active tab, selection, wrap'
  $d = New-DataDir s1
  $r = Start-Ryad $d
  [RyadWin]::Paste((Editor $r), $draft)
  Send-Command $r $CMD.New
  [RyadWin]::Paste((Editor $r), 'друга вкладка')
  Send-Command $r $CMD.PrevTab
  [RyadWin]::SetSel((Editor $r), 3, 9)
  Send-Command $r $CMD.Wrap
  $code = Close-Ryad $r
  Check 'exit code 0' ($code -eq 0)
  $r = Start-Ryad $d
  $e = Editor $r
  Check 'two tabs back' ((Tabs $r) -eq 2)
  Check 'active tab and its text' ([RyadWin]::Text($e) -eq $draftCtl) ([RyadWin]::Text($e))
  $sel = [RyadWin]::Sel($e)
  Check 'selection 3..9' ($sel[0] -eq 3 -and $sel[1] -eq 9) "$($sel[0])..$($sel[1])"
  Check 'wrap kept in the session' ((Index $d) -match '(?s)\[tab\].*?wrap = 1')
  Check 'title marks unsaved text' ([RyadWin]::Title($r.Window).StartsWith('● '))
  # scroll: a long draft scrolled down comes back at the same first line
  Send-Command $r $CMD.New
  $e = Editor $r
  [RyadWin]::Paste($e, ((1..300 | ForEach-Object { "рядок $_" }) -join "`r`n"))
  [RyadWin]::SetSel($e, 0, 0)
  [RyadWin]::ScrollLines($e, 120)
  Start-Sleep -Milliseconds 200
  $top = [RyadWin]::FirstVisible($e)
  [void](Close-Ryad $r)
  $r = Start-Ryad $d
  Start-Sleep -Milliseconds 300   # the scroll is restored once rich edit has laid out
  Check 'scroll position back (first visible line)' ($top -gt 100 -and [RyadWin]::FirstVisible((Editor $r)) -eq $top) "$top -> $([RyadWin]::FirstVisible((Editor $r)))"
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d
}

if (Want 's2') {
  Write-Output 's2 killed in the middle of a commit: last complete generation, user files untouched'
  $d = New-DataDir s2; $f = New-DataDir s2files
  $a = Join-Path $f 'a.txt'; $b = Join-Path $f 'b з пробілом.log'
  Write-File $a "файл a`r`nрядок 2`r`n"; Write-File $b "file b`n"
  $a0 = Read-Bytes $a; $b0 = Read-Bytes $b
  $r = Start-Ryad $d @("`"$a`"", "`"$b`"")
  foreach ($t in 'перша', 'друга', 'третя') { Send-Command $r $CMD.New; [RyadWin]::Paste((Editor $r), "чернетка $t") }
  Send-Command $r ($CMD.TabBase + 0)
  [RyadWin]::Paste((Editor $r), 'ПРАВКА 1 ')
  [void](Close-Ryad $r)
  $gen1 = Index $d
  # run 2: edits, then the process dies after writing snapshots, before the index
  $r = Start-Ryad $d @() @{ RYAD_TEST_FAULT = 'after-snapshots' }
  [RyadWin]::Paste((Editor $r), 'ПРАВКА 2 ')
  $r.Process.WaitForExit(5000) | Out-Null
  Check 'process died in the commit (91)' ($r.Process.ExitCode -eq 91) "$($r.Process.ExitCode)"
  Check 'index unchanged' ((Index $d) -eq $gen1)
  $r = Start-Ryad $d
  Check 'five tabs back' ((Tabs $r) -eq 5) "$(Tabs $r)"
  Check 'last complete generation (edit 1, not edit 2)' ([RyadWin]::Text((Editor $r)).StartsWith('ПРАВКА 1 файл a')) ([RyadWin]::Text((Editor $r)))
  Check 'user file a untouched' (Same-Bytes (Read-Bytes $a) $a0)
  Check 'user file b untouched' (Same-Bytes (Read-Bytes $b) $b0)
  # run 3 dies right after replacing the index: the new generation is complete
  [RyadWin]::Paste((Editor $r), 'ПРАВКА 3 ')
  [void](Close-Ryad $r)
  $r = Start-Ryad $d @() @{ RYAD_TEST_FAULT = 'after-index' }
  [RyadWin]::Paste((Editor $r), 'ПРАВКА 4 ')
  $r.Process.WaitForExit(5000) | Out-Null
  $r = Start-Ryad $d
  Check 'after-index fault: newest generation' ([RyadWin]::Text((Editor $r)).StartsWith('ПРАВКА 1 ПРАВКА 3 ПРАВКА 4 файл a')) ([RyadWin]::Text((Editor $r)))
  [void](Close-Ryad $r)
  $orphans = Get-ChildItem (Join-Path $d 'session') -Filter '*.tmp'
  Check 'no temp files left' ($orphans.Count -eq 0)
  Remove-Item -Recurse -Force $d, $f
}

if (Want 's3') {
  Write-Output 's3 edited file, window closed: edits come back as a local copy, file unchanged'
  $d = New-DataDir s3; $f = New-DataDir s3files
  $a = Join-Path $f 'нотатки без розширення'
  [IO.File]::WriteAllBytes($a, [byte[]](0xEF, 0xBB, 0xBF) + $utf8.GetBytes("з bom`r`nрядок`r`n"))
  $a0 = Read-Bytes $a
  $r = Start-Ryad $d @("`"$a`"")
  [RyadWin]::SetSel((Editor $r), 0, 0)
  [RyadWin]::Paste((Editor $r), 'нове ')
  [void](Close-Ryad $r)
  Check 'file unchanged on disk' (Same-Bytes (Read-Bytes $a) $a0)
  $r = Start-Ryad $d
  Check 'edits back' ([RyadWin]::Text((Editor $r)) -eq "нове з bom`rрядок`r")
  Check 'marked unsaved' ([RyadWin]::Title($r.Window).StartsWith('● '))
  # ctrl+s writes them, with the bom and crlf of the original
  Send-Command $r $CMD.Save
  $want = [byte[]](0xEF, 0xBB, 0xBF) + $utf8.GetBytes("нове з bom`r`nрядок`r`n")
  Check 'saved with bom and crlf' (Same-Bytes (Read-Bytes $a) $want)
  Check 'clean after save' (-not [RyadWin]::Title($r.Window).StartsWith('● '))
  [void](Close-Ryad $r)
  $m = Join-Path $f 'mixed.txt'
  [IO.File]::WriteAllBytes($m, $utf8.GetBytes("a`r`nb`nc`rd`r`r`ne"))
  $m0 = Read-Bytes $m
  $t0 = (Get-Item $m).LastWriteTimeUtc
  $r = Start-Ryad $d @("`"$m`"")
  Send-Command $r $CMD.Save
  Start-Sleep -Milliseconds 200
  Check 'mixed endings, ctrl+s without edits: file not touched' ((Same-Bytes (Read-Bytes $m) $m0) -and (Get-Item $m).LastWriteTimeUtc -eq $t0)
  [void](Close-Ryad $r)
  # a failed save of an existing file: text, dot, original bytes, no temp file
  $r = Start-Ryad $d @() @{ RYAD_TEST_SAVE_FAIL = 'replace' }
  Send-Command $r ($CMD.TabBase + 1)
  [RyadWin]::Paste((Editor $r), 'X')
  Post-Command $r $CMD.Save
  Check 'failed save: error dialog' (Click-Dialog $r 2)
  Check 'failed save: text and dot stay' ([RyadWin]::Title($r.Window).StartsWith('● ') -and [RyadWin]::Text((Editor $r)).Contains('X'))
  Check 'failed save: original bytes, no temp file' ((Same-Bytes (Read-Bytes $m) $m0) -and @(Get-ChildItem $f -Filter '*.tmp' -Force).Count -eq 0)
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d, $f
}

if (Want 's4') {
  Write-Output 's4 ctrl+s interrupted before the session heard of it: no phantom edits'
  $d = New-DataDir s4; $f = New-DataDir s4files
  $a = Join-Path $f 'a.txt'; Write-File $a "один`r`n"
  $r = Start-Ryad $d @("`"$a`"") @{ RYAD_TEST_FAULT = 'after-snapshots@3' }
  Wait-Commit                                  # commit 1: the opened file
  [RyadWin]::Paste((Editor $r), 'ДВА ')
  Wait-Commit                                  # commit 2: the edit as a snapshot
  Send-Command $r $CMD.Save                     # file written; commit 3 dies
  $r.Process.WaitForExit(5000) | Out-Null
  Check 'died in commit 3' ($r.Process.ExitCode -eq 91) "$($r.Process.ExitCode)"
  Check 'file has the edit' ([IO.File]::ReadAllText($a) -eq "ДВА один`r`n")
  $r = Start-Ryad $d
  Check 'text is the saved text' ([RyadWin]::Text((Editor $r)) -eq "ДВА один`r")
  Check 'not shown as unsaved' (-not [RyadWin]::Title($r.Window).StartsWith('● ')) ([RyadWin]::Title($r.Window))
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d, $f
}

if (Want 's5') {
  Write-Output 's5 closing a changed tab: cancel, keep as draft, discard, save, failed save'
  $d = New-DataDir s5; $f = New-DataDir s5files
  $r = Start-Ryad $d
  [RyadWin]::Paste((Editor $r), 'лишити як чернетку')
  Post-Command $r $CMD.CloseTab
  Check 'dialog shown, cancel' (Click-Dialog $r 2)
  Check 'tab stays after cancel' ((Tabs $r) -eq 1 -and [RyadWin]::Text((Editor $r)) -eq 'лишити як чернетку')
  Post-Command $r $CMD.CloseTab
  [void](Click-Dialog $r 102)
  Start-Sleep -Milliseconds 300
  Check 'kept: tab gone, a new blank draft' ([RyadWin]::Text((Editor $r)) -eq '')
  Check 'kept draft in the index' ((Index $d) -match '\[kept\]')
  # discard: confirm, then nothing leads back to the text
  [RyadWin]::Paste((Editor $r), 'СЕКРЕТ для видалення')
  Wait-Commit
  $idLine = ((Index $d) -split "`n" | Where-Object { $_ -match '^snapshot = ' } | Select-Object -Last 1)
  Post-Command $r $CMD.CloseTab
  [void](Click-Dialog $r 103)
  [void](Click-Dialog $r 103)
  Start-Sleep -Milliseconds 500
  $all = (Get-ChildItem (Join-Path $d 'session') -File | Where-Object { $_.Name -ne 'lock' } | ForEach-Object { [IO.File]::ReadAllText($_.FullName) }) -join "`n"
  Check 'discarded text is nowhere in the session folder' (-not $all.Contains('СЕКРЕТ'))
  Send-Command $r $CMD.ReopenClosed
  Start-Sleep -Milliseconds 300
  Check 'ctrl+shift+t brings back the kept draft, not the discarded one' ([RyadWin]::Text((Editor $r)) -eq 'лишити як чернетку') ([RyadWin]::Text((Editor $r)))
  # after a restart the drafts menu brings a kept draft back
  Post-Command $r $CMD.CloseTab
  [void](Click-Dialog $r 102)
  Start-Sleep -Milliseconds 300
  [void](Close-Ryad $r)
  $r = Start-Ryad $d
  Send-Command $r 3000
  Start-Sleep -Milliseconds 300
  Check 'drafts menu after a restart: the kept draft opens' ([RyadWin]::Text((Editor $r)) -eq 'лишити як чернетку') ([RyadWin]::Text((Editor $r)))
  # keep as draft while the session folder cannot be written: the tab stays
  [RyadWin]::Paste((Editor $r), 'Б')
  Wait-Commit
  $acl = Get-Acl (Join-Path $d 'session')
  $deny = New-Object Security.AccessControl.FileSystemAccessRule([Security.Principal.WindowsIdentity]::GetCurrent().Name, 'CreateFiles,WriteData,AppendData,Delete', 'Deny')
  $acl.AddAccessRule($deny); Set-Acl (Join-Path $d 'session') $acl
  $before = Tabs $r
  Post-Command $r $CMD.CloseTab
  [void](Click-Dialog $r 102)
  Check 'keep as draft fails: the problem is shown' (Click-Dialog $r 101)
  Check 'keep as draft fails: the tab and its text stay' ((Tabs $r) -eq $before -and [RyadWin]::Text((Editor $r)).Contains('лишити як чернетку')) "$(Tabs $r)"
  $acl.RemoveAccessRule($deny) | Out-Null; Set-Acl (Join-Path $d 'session') $acl
  [void](Close-Ryad $r)
  $r = Start-Ryad $d
  # save a draft through the close question (the dialog path comes from the test hook)
  $target = Join-Path $f 'збережена чернетка.txt'
  [void](Close-Ryad $r)
  $r = Start-Ryad $d @() @{ RYAD_TEST_SAVE_PATH = $target }
  $tabsBefore = Tabs $r
  Post-Command $r $CMD.CloseTab
  [void](Click-Dialog $r 101)
  Start-Sleep -Milliseconds 300
  Check 'saved to the chosen file' ((Test-Path $target) -and [IO.File]::ReadAllText($target).StartsWith('лишити як чернетку'))
  [void](Close-Ryad $r)
  # save as cancelled (empty path) and save into a missing folder: the tab stays changed
  $r = Start-Ryad $d @() @{ RYAD_TEST_SAVE_PATH = '' }
  [RyadWin]::Paste((Editor $r), 'не збережено')
  Send-Command $r $CMD.Save
  Check 'cancelled save as: still unsaved' ([RyadWin]::Title($r.Window).StartsWith('● '))
  [void](Close-Ryad $r)
  $r = Start-Ryad $d @() @{ RYAD_TEST_SAVE_PATH = (Join-Path $f 'немає\x.txt') }
  Post-Command $r $CMD.Save
  Check 'error dialog for a missing folder' (Click-Dialog $r 2)
  Check 'text and dot stay after the failure' ([RyadWin]::Title($r.Window).StartsWith('● ') -and [RyadWin]::Text((Editor $r)).Contains('не збережено'))
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d, $f
}

if (Want 's6') {
  Write-Output 's6 file changed by another program: detected before overwrite'
  $d = New-DataDir s6; $f = New-DataDir s6files
  $a = Join-Path $f 'a.txt'; Write-File $a "оригінал`r`n"
  $r = Start-Ryad $d @("`"$a`"")
  [RyadWin]::Paste((Editor $r), 'МОЄ ')
  Start-Sleep -Milliseconds 1100
  Write-File $a "чужа правка`r`n"
  Post-Command $r $CMD.Save
  Check 'conflict dialog, reload' (Click-Dialog $r 102)
  Start-Sleep -Milliseconds 200
  Check 'reloaded text from disk' ([RyadWin]::Text((Editor $r)) -eq "чужа правка`r")
  Check 'disk still has the other program''s text' ([IO.File]::ReadAllText($a) -eq "чужа правка`r`n")
  Send-Command $r 110  # undo
  Check 'ctrl+z brings my edit back' ([RyadWin]::Text((Editor $r)) -eq "МОЄ оригінал`r") ([RyadWin]::Text((Editor $r)))
  Start-Sleep -Milliseconds 1100
  Write-File $a "ще одна чужа правка`r`n"
  Post-Command $r $CMD.Save
  Check 'conflict dialog, overwrite' (Click-Dialog $r 103)
  Check 'file overwritten on purpose' ([IO.File]::ReadAllText($a) -eq "МОЄ оригінал`r`n")
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d, $f
}

if (Want 's7') {
  Write-Output 's7 second start hands paths to the running ryad; one owner'
  $d = New-DataDir s7; $f = New-DataDir s7files
  $a = Join-Path $f 'з пробілом і кирилицею.txt'; Write-File $a 'один'
  $b = Join-Path $f 'second.log'; Write-File $b 'два'
  $c = Join-Path $f 'third.md'; Write-File $c 'три'
  $r = Start-Ryad $d
  $p2 = Start-Process -FilePath $exe -ArgumentList @('--data', "`"$d`"", "`"$a`"") -PassThru
  Check 'second start exits' ($p2.WaitForExit(10000) -and $p2.ExitCode -eq 0)
  Start-Sleep -Milliseconds 400
  Check 'file opened in the first window' ([RyadWin]::Text((Editor $r)) -eq 'один')
  # two starts at the same moment, while ryad runs
  $p3 = Start-Process -FilePath $exe -ArgumentList @('--data', "`"$d`"", "`"$b`"") -PassThru
  $p4 = Start-Process -FilePath $exe -ArgumentList @('--data', "`"$d`"", "`"$c`"") -PassThru
  [void]$p3.WaitForExit(10000); [void]$p4.WaitForExit(10000)
  Start-Sleep -Milliseconds 500
  $texts = [RyadWin]::Editors($r.Window) | ForEach-Object { [RyadWin]::Text($_) }
  Check 'both files arrived' (($texts -contains 'два') -and ($texts -contains 'три')) ($texts -join '|')
  $p5 = Start-Process -FilePath $exe -ArgumentList @('--data', "`"$d`"", "`"$a`"") -PassThru
  [void]$p5.WaitForExit(10000); Start-Sleep -Milliseconds 400
  Check 'the same file again focuses its tab' ((Tabs $r) -eq 3 -and [RyadWin]::Text((Editor $r)) -eq 'один') "$(Tabs $r)"
  [void](Close-Ryad $r)
  # cold: two starts at once, nobody running yet
  $q1 = Start-Process -FilePath $exe -ArgumentList @('--data', "`"$d`"", "`"$b`"") -PassThru
  $q2 = Start-Process -FilePath $exe -ArgumentList @('--data', "`"$d`"", "`"$c`"") -PassThru
  Start-Sleep -Seconds 2
  $alive = @($q1, $q2) | Where-Object { -not $_.HasExited }
  Check 'one owner after a simultaneous cold start' ($alive.Count -eq 1) "$($alive.Count)"
  if ($alive.Count -ge 1) {
    $w = [RyadWin]::MainWindow([uint32]$alive[0].Id)
    $texts = [RyadWin]::Editors($w) | ForEach-Object { [RyadWin]::Text($_) }
    Check 'both paths in that one window' (($texts -contains 'два') -and ($texts -contains 'три')) ($texts -join '|')
    [void][RyadWin]::PostMessage($w, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)
    [void]$alive[0].WaitForExit(10000)
  }
  Remove-Item -Recurse -Force $d, $f
}

if (Want 's8') {
  Write-Output 's8 a file removed while ryad was closed'
  $d = New-DataDir s8; $f = New-DataDir s8files
  $a = Join-Path $f 'a.txt'; Write-File $a 'текст'
  $r = Start-Ryad $d @("`"$a`"")
  [void](Close-Ryad $r)
  Remove-Item $a
  $r = Start-Ryad $d
  $banner = [RyadWin]::Children($r.Window, 'ryad.banner') | Where-Object { [RyadWin]::IsWindowVisible($_) }
  Check 'tab kept with a notice' ((Tabs $r) -eq 1 -and @($banner).Count -eq 1)
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d, $f
}

if (Want 's9') {
  Write-Output 's9 find, replace, go to line'
  $d = New-DataDir s9; $f = New-DataDir s9files
  $a = Join-Path $f 'пошук.txt'
  Write-File $a "Кіт і кит`r`nкіт`r`nостанній рядок КІТ"
  $r = Start-Ryad $d @("`"$a`"")
  $e = Editor $r
  Send-Command $r $CMD.Find
  $find = [RyadWin]::Children($r.Window, 'Edit') | Select-Object -First 1
  $replace = [RyadWin]::Children($r.Window, 'Edit') | Select-Object -Last 1
  function SetFind([string] $t) { [void][RyadWin]::SendMessage($find, 0x000C, [IntPtr]::Zero, $t) }
  SetFind 'кіт'
  [RyadWin]::SetSel($e, 0, 0)
  Send-Command $r $CMD.FindNext
  Check 'cyrillic, any case: first match "Кіт"' (([RyadWin]::Sel($e) -join ',') -eq '0,3') ([RyadWin]::Sel($e) -join ',')
  Send-Command $r $CMD.FindNext
  Check 'next: "кіт" on line 2' (([RyadWin]::Sel($e) -join ',') -eq '10,13') ([RyadWin]::Sel($e) -join ',')
  Send-Command $r $CMD.FindNext
  Check 'next: "КІТ" at the very end' (([RyadWin]::Sel($e) -join ',') -eq '29,32') ([RyadWin]::Sel($e) -join ',')
  Send-Command $r $CMD.FindNext
  Check 'past the end: from the top again' (([RyadWin]::Sel($e) -join ',') -eq '0,3') ([RyadWin]::Sel($e) -join ',')
  Send-Command $r 122  # find previous
  Check 'previous from the first: the last one' (([RyadWin]::Sel($e) -join ',') -eq '29,32') ([RyadWin]::Sel($e) -join ',')
  # match case through the panel's button
  $case = [RyadWin]::Children($r.Window, 'Button') | Where-Object { [RyadWin]::Title($_) -eq 'match case' }
  [void][RyadWin]::SendMessage($case, 0x00F5, [IntPtr]::Zero, [IntPtr]::Zero) # BM_CLICK
  [RyadWin]::SetSel($e, 0, 0)
  Send-Command $r $CMD.FindNext
  Check 'match case: skips "Кіт"' (([RyadWin]::Sel($e) -join ',') -eq '10,13') ([RyadWin]::Sel($e) -join ',')
  SetFind 'собака'
  [RyadWin]::SetSel($e, 0, 0)
  Send-Command $r $CMD.FindNext
  Check 'no match: selection stays' (([RyadWin]::Sel($e) -join ',') -eq '0,0')
  # replace all: every match once, one undo step
  [void][RyadWin]::SendMessage($case, 0x00F5, [IntPtr]::Zero, [IntPtr]::Zero)
  Send-Command $r $CMD.Replace
  SetFind 'кіт'
  [void][RyadWin]::SendMessage($replace, 0x000C, [IntPtr]::Zero, 'кіт-кіт')
  $all = [RyadWin]::Children($r.Window, 'Button') | Where-Object { [RyadWin]::Title($_) -eq 'replace all' }
  [void][RyadWin]::SendMessage($all, 0x00F5, [IntPtr]::Zero, [IntPtr]::Zero)
  Check 'replace all: each match once' ([RyadWin]::Text($e) -eq "кіт-кіт і кит`rкіт-кіт`rостанній рядок кіт-кіт") ([RyadWin]::Text($e))
  Send-Command $r 110  # undo
  Check 'one undo restores everything' ([RyadWin]::Text($e) -eq "Кіт і кит`rкіт`rостанній рядок КІТ") ([RyadWin]::Text($e))
  # go to line: the last line, and a number past the end
  Send-Command $r $CMD.GoTo
  SetFind '3'
  [void][RyadWin]::SendMessage(([RyadWin]::Children($r.Window, 'ryad.find') | Select-Object -First 1), 0x0111, [IntPtr]1, [IntPtr]::Zero) # enter
  Check 'go to line 3' (([RyadWin]::Sel($e) -join ',') -eq '14,14') ([RyadWin]::Sel($e) -join ',')
  Send-Command $r $CMD.GoTo
  SetFind '99'
  [void][RyadWin]::SendMessage(([RyadWin]::Children($r.Window, 'ryad.find') | Select-Object -First 1), 0x0111, [IntPtr]1, [IntPtr]::Zero)
  Check 'line past the end: the last line' (([RyadWin]::Sel($e) -join ',') -eq '14,14') ([RyadWin]::Sel($e) -join ',')
  # wrap on: the same logical lines
  Send-Command $r $CMD.Wrap
  Send-Command $r $CMD.GoTo
  SetFind '2'
  [void][RyadWin]::SendMessage(([RyadWin]::Children($r.Window, 'ryad.find') | Select-Object -First 1), 0x0111, [IntPtr]1, [IntPtr]::Zero)
  Check 'go to line 2 with wrap on' (([RyadWin]::Sel($e) -join ',') -eq '10,10') ([RyadWin]::Sel($e) -join ',')
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d, $f
}

if (Want 's10') {
  Write-Output 's10 keys through the message loop (posted key messages, not WM_COMMAND)'
  $d = New-DataDir s10
  $r = Start-Ryad $d
  $e = Editor $r
  [RyadWin]::Paste($e, 'перший')
  [void][RyadWin]::PostMessage($e, 0x0100, [IntPtr]0x0D, [IntPtr]0x001C0001)  # WM_KEYDOWN enter
  [void][RyadWin]::PostMessage($e, 0x0101, [IntPtr]0x0D, [IntPtr]0xC01C0001)  # WM_KEYUP
  Start-Sleep -Milliseconds 200
  [RyadWin]::Paste($e, 'другий')
  Check 'enter makes a new line' ([RyadWin]::Text($e) -eq "перший`rдругий") ([RyadWin]::Text($e))
  Send-Command $r $CMD.Find
  $find = [RyadWin]::Children($r.Window, 'Edit') | Select-Object -First 1
  [void][RyadWin]::SendMessage($find, 0x000C, [IntPtr]::Zero, 'и')
  Check 'ctrl+f puts the focus into the find field' ([RyadWin]::Focus($r.Window) -eq $find)
  [void][RyadWin]::PostMessage($find, 0x0100, [IntPtr]0x1B, [IntPtr]0x00010001)  # esc
  Start-Sleep -Milliseconds 200
  $panel = [RyadWin]::Children($r.Window, 'ryad.find') | Select-Object -First 1
  Check 'esc closes the find bar' (-not [RyadWin]::IsWindowVisible($panel))
  Check 'and the focus is back in the text' ([RyadWin]::Focus($r.Window) -eq $e)
  [RyadWin]::SetSel($e, 0, 0)
  [void][RyadWin]::PostMessage($e, 0x0100, [IntPtr]0x72, [IntPtr]0x003D0001)  # f3 (accelerator)
  Start-Sleep -Milliseconds 200
  Check 'f3 finds the next match' (([RyadWin]::Sel($e) -join ',') -eq '4,5') ([RyadWin]::Sel($e) -join ',')
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d
}

if (Want 's11') {
  Write-Output 's11 markdown preview: on for .md, toggled per tab, kept in the session'
  $d = New-DataDir s11; $f = New-DataDir s11files
  $md = Join-Path $f 'нотатки.md'; Write-File $md "# заголовок`n`n- пункт`n`n``код``"
  $txt = Join-Path $f 'plain.txt'; Write-File $txt 'текст'
  $r = Start-Ryad $d @("`"$md`"", "`"$txt`"")
  $pv = [RyadWin]::Preview($r.Window)
  Check 'a .txt tab has no preview' (-not [RyadWin]::IsWindowVisible($pv))
  Send-Command $r ($CMD.TabBase + 0)
  Start-Sleep -Milliseconds 300
  Check 'a .md tab shows the preview' ([RyadWin]::IsWindowVisible($pv))
  Check 'the preview shows the rendered text' ([RyadWin]::Text($pv).Contains('заголовок') -and -not [RyadWin]::Text($pv).Contains('#'))
  [RyadWin]::Paste((Editor $r), "## ще`r`n")
  Start-Sleep -Milliseconds 600
  Check 'typing updates the preview after a pause' ([RyadWin]::Text($pv).Contains('ще'))
  Send-Command $r 134
  Check 'ctrl+shift+m hides it' (-not [RyadWin]::IsWindowVisible($pv))
  [void](Close-Ryad $r)
  $r = Start-Ryad $d
  $pv = [RyadWin]::Preview($r.Window)
  Check 'off after a restart' (-not [RyadWin]::IsWindowVisible($pv))
  Send-Command $r 134
  [void](Close-Ryad $r)
  $r = Start-Ryad $d
  $pv = [RyadWin]::Preview($r.Window)
  Start-Sleep -Milliseconds 300
  Check 'on after a restart' ([RyadWin]::IsWindowVisible($pv))
  [void](Close-Ryad $r)
  Remove-Item -Recurse -Force $d, $f
}

Write-Output "passed $script:pass, failed $script:fail"
if ($script:fail) { exit 1 }
