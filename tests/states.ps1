# screenshots of the states the brief asks to see live (§6.3), of ryad's own
# window only (PrintWindow). isolated data folder, synthetic files.
#   powershell -NoProfile -ExecutionPolicy Bypass -File tests\states.ps1 -Out <folder> [-Theme dark|light]
param([string] $Out, [string] $Theme = '')
. (Join-Path $PSScriptRoot 'common.ps1')
New-Item -ItemType Directory -Force $Out | Out-Null
$envs = @{}; if ($Theme) { $envs['RYAD_THEME'] = $Theme }
$suffix = if ($Theme) { "-$Theme" } else { '' }

# 1. first start: an empty window to type into
$d = New-DataDir states
$r = Start-Ryad $d @() $envs
Start-Sleep -Milliseconds 300
Save-Shot $r (Join-Path $Out "01-first-start$suffix.png")

# 2. active, inactive, changed tabs; many tabs (overflow), hidden active tab
$f = New-DataDir statesfiles
$names = 'readme.md', 'server.log', 'config.ini', 'дуже довга назва файла з кирилицею.txt', 'notes.txt', 'data.json', 'report.csv', 'build.log', 'todo.txt', 'app.cfg', 'страница.xml'
$paths = foreach ($n in $names) { $p = Join-Path $f $n; [IO.File]::WriteAllText($p, "file $n`r`nline 2`r`n"); $p }
foreach ($p in $paths[0..2]) { $p2 = Start-Process -FilePath $exe -ArgumentList @('--data', "`"$d`"", "`"$p`"") -PassThru; [void]$p2.WaitForExit(5000) }
Start-Sleep -Milliseconds 400
Send-Command $r ($CMD.TabBase + 2)
[RyadWin]::Paste([RyadWin]::VisibleEditor($r.Window), 'змінено ')
Send-Command $r ($CMD.TabBase + 1)
Start-Sleep -Milliseconds 300
Save-Shot $r (Join-Path $Out "02-tabs$suffix.png")
foreach ($p in $paths[3..10]) { $p2 = Start-Process -FilePath $exe -ArgumentList @('--data', "`"$d`"", "`"$p`"") -PassThru; [void]$p2.WaitForExit(5000) }
Start-Sleep -Milliseconds 600
Save-Shot $r (Join-Path $Out "03-overflow$suffix.png")

# 3. find with a result and without
Send-Command $r ($CMD.TabBase + 1)
Send-Command $r $CMD.Find
$find = [RyadWin]::Children($r.Window, 'Edit') | Select-Object -First 1
[void][RyadWin]::SendMessage($find, 0x000C, [IntPtr]::Zero, 'line')
Send-Command $r $CMD.FindNext
Start-Sleep -Milliseconds 200
Save-Shot $r (Join-Path $Out "04-find$suffix.png")
[void][RyadWin]::SendMessage($find, 0x000C, [IntPtr]::Zero, 'немає такого')
Send-Command $r $CMD.FindNext
Start-Sleep -Milliseconds 200
Save-Shot $r (Join-Path $Out "05-find-none$suffix.png")
Send-Command $r $CMD.Replace
Start-Sleep -Milliseconds 200
Save-Shot $r (Join-Path $Out "06-replace$suffix.png")
[void](Close-Ryad $r)

# 4. a file that is gone, a file changed elsewhere
Remove-Item $paths[1]
$r = Start-Ryad $d @() $envs
Send-Command $r ($CMD.TabBase + 1)
Start-Sleep -Milliseconds 300
Save-Shot $r (Join-Path $Out "07-missing$suffix.png")
[void](Close-Ryad $r)

# 5. the session folder cannot be written: the red banner
$r = Start-Ryad $d @() $envs
$sess = Join-Path $d 'session\session.ini'
$acl = Get-Acl (Join-Path $d 'session')
$deny = New-Object Security.AccessControl.FileSystemAccessRule([Security.Principal.WindowsIdentity]::GetCurrent().Name, 'CreateFiles,WriteData,AppendData,Delete', 'Deny')
$acl.AddAccessRule($deny); Set-Acl (Join-Path $d 'session') $acl
[RyadWin]::Paste([RyadWin]::VisibleEditor($r.Window), 'не вдасться записати ')
Start-Sleep -Milliseconds 1500
Save-Shot $r (Join-Path $Out "08-session-error$suffix.png")
$acl.RemoveAccessRule($deny) | Out-Null; Set-Acl (Join-Path $d 'session') $acl
Send-Command $r 160  # banner: retry
Start-Sleep -Milliseconds 800
Save-Shot $r (Join-Path $Out "09-session-recovered$suffix.png")
[void](Close-Ryad $r)
Remove-Item -Recurse -Force $d, $f
