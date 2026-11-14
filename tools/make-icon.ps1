# draws assets\ryad.ico: the ryad mark (three rows of text) on a graphite
# tile, at every size windows asks for. tile, sizes and file writing come from
# kadr tools\make-icon.ps1 (29db81d, MIT). run it again only to change the icon.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\make-icon.ps1 [-Preview <png>]
param([string] $Preview = '')
Add-Type -AssemblyName System.Drawing
$root = Split-Path -Parent $PSScriptRoot
$out = Join-Path $root 'assets\ryad.ico'
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null

$tile = [Drawing.Color]::FromArgb(255, 0x1c, 0x1c, 0x1e)   # graphite ground
$edge = [Drawing.Color]::FromArgb(255, 0x48, 0x48, 0x4a)   # keeps the tile apart from a dark taskbar
$ink = [Drawing.Color]::FromArgb(255, 0xf2, 0xf2, 0xf2)

function Draw([int] $s) {
  $bmp = New-Object Drawing.Bitmap $s, $s, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [Drawing.Graphics]::FromImage($bmp)
  $g.Clear([Drawing.Color]::Transparent)
  $g.SmoothingMode = 'AntiAlias'
  $g.PixelOffsetMode = 'Half'
  # rounded tile, windows 11 proportions
  $r = [math]::Max(2.0, $s * 0.22)
  $path = New-Object Drawing.Drawing2D.GraphicsPath
  $d = 2 * $r; $w = $s - 1
  $path.AddArc(0, 0, $d, $d, 180, 90); $path.AddArc($w - $d, 0, $d, $d, 270, 90)
  $path.AddArc($w - $d, $w - $d, $d, $d, 0, 90); $path.AddArc(0, $w - $d, $d, $d, 90, 90); $path.CloseFigure()
  $g.FillPath((New-Object Drawing.SolidBrush $tile), $path)
  if ($s -ge 24) { $g.DrawPath((New-Object Drawing.Pen $edge, 1), $path) }
  # the ryad mark: three rows of text, left-aligned, the last one shorter,
  # on whole pixels so small sizes stay sharp
  $g.SmoothingMode = 'None'; $g.PixelOffsetMode = 'None'
  $b = New-Object Drawing.SolidBrush $ink
  $t = [math]::Max(2, [math]::Round($s * 0.09))
  $left = [math]::Max(3, [math]::Round($s * 0.24))
  $right = $s - $left
  $gap = [math]::Max(2, [math]::Round(($s - 2 * $left - 3 * $t) / 2.0))
  $top = [math]::Floor(($s - (3 * $t + 2 * $gap)) / 2)
  $lens = @(($right - $left), ($right - $left), [math]::Max($t * 2, [math]::Round(($right - $left) * 0.55)))
  for ($i = 0; $i -lt 3; $i++) {
    $g.FillRectangle($b, $left, $top + $i * ($t + $gap), $lens[$i], $t)
  }
  $g.Dispose()
  $bmp
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 96, 256
$images = foreach ($s in $sizes) {
  $bmp = Draw $s
  $ms = New-Object IO.MemoryStream
  $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  [pscustomobject]@{ size = $s; png = $ms.ToArray() }
}

# .ico with png entries (supported for every size since windows vista)
$fs = [IO.File]::Create($out); $bw = New-Object IO.BinaryWriter $fs
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($i in $images) {
  $dim = if ($i.size -ge 256) { 0 } else { $i.size }
  $bw.Write([byte]$dim); $bw.Write([byte]$dim); $bw.Write([byte]0); $bw.Write([byte]0)
  $bw.Write([uint16]1); $bw.Write([uint16]32); $bw.Write([uint32]$i.png.Length); $bw.Write([uint32]$offset)
  $offset += $i.png.Length
}
foreach ($i in $images) { $bw.Write($i.png) }
$bw.Close()
Write-Host "wrote $out ($((Get-Item $out).Length) bytes, sizes $($sizes -join ', '))"

if ($Preview) {
  # every size at 1:1 on light and dark grounds, plus 16 and 32 enlarged 8x
  $W = 900; $H = 420
  $sheet = New-Object Drawing.Bitmap $W, $H
  $g = [Drawing.Graphics]::FromImage($sheet)
  $g.FillRectangle((New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(243, 243, 243))), 0, 0, $W, 140)
  $g.FillRectangle((New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(32, 32, 32))), 0, 140, $W, 140)
  $x = 10
  foreach ($i in $images) {
    $img = [Drawing.Image]::FromStream((New-Object IO.MemoryStream(, $i.png)))
    $sz = [math]::Min($i.size, 128)
    $g.DrawImage($img, $x, 10, $sz, $sz); $g.DrawImage($img, $x, 150, $sz, $sz)
    $x += $sz + 12
  }
  $g.InterpolationMode = 'NearestNeighbor'; $g.PixelOffsetMode = 'Half'
  foreach ($pair in @(@(16, 10), @(32, 160), @(24, 450))) {
    $i = $images | Where-Object size -eq $pair[0]
    $img = [Drawing.Image]::FromStream((New-Object IO.MemoryStream(, $i.png)))
    $g.DrawImage($img, $pair[1], 290, [math]::Min(128, $pair[0] * 8), [math]::Min(128, $pair[0] * 8))
  }
  $g.Dispose(); $sheet.Save($Preview, [Drawing.Imaging.ImageFormat]::Png); $sheet.Dispose()
  Write-Host "preview $Preview"
}
