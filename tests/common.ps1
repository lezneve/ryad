# shared helpers for ryad's scripted checks. they drive ryad only with
# messages to its own windows (WM_COMMAND, WM_CHAR, WM_CLOSE...), never with
# synthetic keyboard or mouse input, and always with an isolated data folder
# (--data), so the user's real drafts are never read or touched.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
# RYAD_EXE runs the checks against another build, e.g. the one in dist\
$exe = if ($env:RYAD_EXE) { $env:RYAD_EXE } else { Join-Path $root 'target\release\ryad.exe' }

Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public static class RyadWin {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr p, EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, StringBuilder l);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, string l);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int hh, uint f);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [StructLayout(LayoutKind.Sequential)] public struct CHARRANGE { public int Min, Max; }
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, uint m, IntPtr w, ref CHARRANGE l);

  public static IntPtr MainWindow(uint pid) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      var c = new StringBuilder(256); GetClassName(h, c, 256);
      if (p == pid && c.ToString().StartsWith("ryad.main.")) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
  public static List<IntPtr> Children(IntPtr parent, string cls) {
    var list = new List<IntPtr>();
    EnumChildWindows(parent, (h, l) => {
      var c = new StringBuilder(256); GetClassName(h, c, 256);
      if (c.ToString() == cls) list.Add(h);
      return true;
    }, IntPtr.Zero);
    return list;
  }
  public static IntPtr VisibleEditor(IntPtr main) {
    // the markdown preview is a read-only rich edit (ES_READONLY): not a text field
    foreach (var h in Children(main, "RICHEDIT50W")) if (IsWindowVisible(h) && (GetWindowLong(h, -16) & 0x800) == 0) return h;
    return IntPtr.Zero;
  }
  [DllImport("user32.dll")] public static extern int GetWindowLong(IntPtr h, int i);
  public static List<IntPtr> Editors(IntPtr main) {
    var list = new List<IntPtr>();
    foreach (var h in Children(main, "RICHEDIT50W")) if ((GetWindowLong(h, -16) & 0x800) == 0) list.Add(h);
    return list;
  }
  public static IntPtr Preview(IntPtr main) {
    foreach (var h in Children(main, "RICHEDIT50W")) if ((GetWindowLong(h, -16) & 0x800) != 0) return h;
    return IntPtr.Zero;
  }
  public static string Text(IntPtr h) {
    int n = (int)SendMessage(h, 0x000E, IntPtr.Zero, IntPtr.Zero); // WM_GETTEXTLENGTH
    var sb = new StringBuilder(n + 2);
    SendMessage(h, 0x000D, (IntPtr)(n + 1), sb); // WM_GETTEXT
    // rich edit hands out crlf through WM_GETTEXT; inside it is one \r
    return sb.ToString().Replace("\r\n", "\r");
  }
  public static string Title(IntPtr h) { var sb = new StringBuilder(512); GetWindowText(h, sb, 512); return sb.ToString(); }
  public static void Type(IntPtr h, string s) {
    foreach (char ch in s) SendMessage(h, 0x0102, (IntPtr)ch, (IntPtr)1); // WM_CHAR
  }
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c);
  [StructLayout(LayoutKind.Sequential)] public struct GUITHREADINFO { public int cbSize, flags; public IntPtr active, focus, capture, menuOwner, moveSize, caret; public RECT rcCaret; }
  [DllImport("user32.dll")] public static extern bool GetGUIThreadInfo(uint thread, ref GUITHREADINFO info);
  public static IntPtr Focus(IntPtr window) { uint pid; uint tid = GetWindowThreadProcessId(window, out pid); var g = new GUITHREADINFO(); g.cbSize = Marshal.SizeOf(g); GetGUIThreadInfo(tid, ref g); return g.focus; }
  public static int FirstVisible(IntPtr h) { return (int)SendMessage(h, 0x00CE, IntPtr.Zero, IntPtr.Zero); } // EM_GETFIRSTVISIBLELINE
  public static void ScrollLines(IntPtr h, int n) { SendMessage(h, 0x00B6, IntPtr.Zero, (IntPtr)n); } // EM_LINESCROLL
  public static void Paste(IntPtr h, string s) { SendMessage(h, 0x00C2, (IntPtr)1, s); } // EM_REPLACESEL, undoable
  // EM_GETSEL / EM_SETSEL take values, not pointers: messages from WM_USER up
  // (EM_EXGETSEL...) are not marshalled between processes, and a pointer
  // from this process would be read inside ryad's.
  public static int[] Sel(IntPtr h) { long v = (long)SendMessage(h, 0x00B0, IntPtr.Zero, IntPtr.Zero); return new int[] { (int)(v & 0xffff), (int)((v >> 16) & 0xffff) }; }
  public static void SetSel(IntPtr h, int a, int b) { SendMessage(h, 0x00B1, (IntPtr)a, (IntPtr)b); }
}
'@

# physical pixels, like ryad itself (per-monitor v2)
[void][RyadWin]::SetThreadDpiAwarenessContext([IntPtr]-4)

function New-DataDir([string] $tag) {
  $d = Join-Path ([IO.Path]::GetTempPath()) ("ryad-test-$tag-" + [guid]::NewGuid().ToString('n').Substring(0, 8))
  New-Item -ItemType Directory -Force $d | Out-Null
  $d
}

function Start-Ryad([string] $data, [string[]] $extra = @(), [hashtable] $env = @{}) {
  $psi = New-Object Diagnostics.ProcessStartInfo $exe
  $psi.Arguments = (@('--data', "`"$data`"") + $extra) -join ' '
  $psi.UseShellExecute = $false
  foreach ($k in $env.Keys) { $psi.EnvironmentVariables[$k] = $env[$k] }
  $p = [Diagnostics.Process]::Start($psi)
  $deadline = (Get-Date).AddSeconds(10)
  do {
    Start-Sleep -Milliseconds 50
    $h = [RyadWin]::MainWindow([uint32]$p.Id)
  } while ($h -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline -and -not $p.HasExited)
  if ($h -eq [IntPtr]::Zero) { throw "ryad window did not appear" }
  # ready once a text field is visible (the window shows before the session is restored)
  while ([RyadWin]::VisibleEditor($h) -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline -and -not $p.HasExited) { Start-Sleep -Milliseconds 20 }
  Start-Sleep -Milliseconds 50
  [pscustomobject]@{ Process = $p; Window = $h }
}

function Send-Command($r, [int] $id) { [void][RyadWin]::SendMessage($r.Window, 0x0111, [IntPtr]$id, [IntPtr]::Zero) }

function Close-Ryad($r, [int] $timeoutMs = 10000) {
  [void][RyadWin]::PostMessage($r.Window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) # WM_CLOSE
  if (-not $r.Process.WaitForExit($timeoutMs)) { throw "ryad did not exit" }
  $r.Process.ExitCode
}

function Save-Shot($r, [string] $png) {
  Add-Type -AssemblyName System.Drawing
  $rc = New-Object RyadWin+RECT
  [void][RyadWin]::GetWindowRect($r.Window, [ref]$rc)
  $w = $rc.Right - $rc.Left; $h = $rc.Bottom - $rc.Top
  $bmp = New-Object Drawing.Bitmap $w, $h
  $g = [Drawing.Graphics]::FromImage($bmp)
  $dc = $g.GetHdc()
  [void][RyadWin]::PrintWindow($r.Window, $dc, 2) # PW_RENDERFULLCONTENT, ryad's window only
  $g.ReleaseHdc($dc); $g.Dispose()
  $bmp.Save($png, [Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
}

$CMD = @{ TabBase = 1000; New = 100; Open = 101; Save = 102; SaveAs = 103; CloseTab = 104; ReopenClosed = 105; Find = 120; FindNext = 121; Replace = 123; GoTo = 124; Wrap = 130; NextTab = 131; PrevTab = 132 }
