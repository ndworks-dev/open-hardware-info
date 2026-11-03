# Generates icon.ico and icon-64.png in this folder. Run it after changing the
# design; the build only consumes the generated files.
#
# The icon is an orange chip with a hot red core. Every size is drawn separately
# instead of scaled from one image, with the pins and the core snapped to whole
# pixels, so it stays sharp at 16 px in the tray.
param([string]$OutDir = $PSScriptRoot)

Add-Type -AssemblyName System.Drawing

function New-RoundedRect([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $path.AddArc($x, $y, $d, $d, 180, 90)
    $path.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $path.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $path.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return , $path
}

function Get-EvenRound([double]$value) {
    return [int]([Math]::Round($value / 2) * 2)
}

function New-IconBitmap([int]$size) {
    $bitmap = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = 'AntiAlias'
    $g.PixelOffsetMode = 'HighQuality'

    # Pins. There is no tile behind the chip, so their mid-tone color has to
    # stand out on both dark and light taskbars.
    $pinLength = [Math]::Max(2, (Get-EvenRound ($size * 0.125)))
    $pinCount = if ($size -ge 32) { 3 } else { 2 }
    $body = $size - 2 * $pinLength
    $pinWidth = [Math]::Max(2, (Get-EvenRound ($body * 0.13)))
    $slot = $body / [double]$pinCount
    $pinBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 245, 150, 60))
    for ($i = 0; $i -lt $pinCount; $i++) {
        $at = $pinLength + [int][Math]::Round($slot * ($i + 0.5) - $pinWidth / 2)
        $g.FillRectangle($pinBrush, $at, 0, $pinWidth, $pinLength + 1)
        $g.FillRectangle($pinBrush, $at, $size - $pinLength - 1, $pinWidth, $pinLength + 1)
        $g.FillRectangle($pinBrush, 0, $at, $pinLength + 1, $pinWidth)
        $g.FillRectangle($pinBrush, $size - $pinLength - 1, $at, $pinLength + 1, $pinWidth)
    }

    # Chip body.
    $bottomLeft = New-Object System.Drawing.PointF $pinLength, ($size - $pinLength)
    $topRight = New-Object System.Drawing.PointF ($size - $pinLength), $pinLength
    $deepOrange = [System.Drawing.Color]::FromArgb(255, 234, 88, 12)
    $amber = [System.Drawing.Color]::FromArgb(255, 245, 158, 11)
    $gradient = New-Object System.Drawing.Drawing2D.LinearGradientBrush $bottomLeft, $topRight, $deepOrange, $amber
    $g.FillPath($gradient, (New-RoundedRect $pinLength $pinLength $body $body ($body * 0.14)))

    # Core: a white socket around the hot center. Even sizes keep both centered,
    # and small sizes use square corners, which would only blur at that scale.
    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
    $red = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 239, 68, 68))
    foreach ($layer in @(@(0.52, $white), @(0.28, $red))) {
        $side = [Math]::Max(2, (Get-EvenRound ($body * $layer[0])))
        $at = ($size - $side) / 2
        if ($size -ge 32) {
            $g.FillPath($layer[1], (New-RoundedRect $at $at $side $side ($side * 0.2)))
        } else {
            $g.FillRectangle($layer[1], $at, $at, $side, $side)
        }
    }

    $g.Dispose()
    return , $bitmap
}

function Get-PngBytes($bitmap) {
    $stream = New-Object System.IO.MemoryStream
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    return , $stream.ToArray()
}

# An uncompressed ICO frame: bottom-up BGRA followed by an all-zero AND mask.
function Get-DibBytes($bitmap) {
    $size = $bitmap.Width
    $stream = New-Object System.IO.MemoryStream
    $writer = New-Object System.IO.BinaryWriter $stream
    $writer.Write([int]40); $writer.Write([int]$size); $writer.Write([int]($size * 2))
    $writer.Write([int16]1); $writer.Write([int16]32); $writer.Write([int]0)
    $writer.Write([int]0); $writer.Write([int]0); $writer.Write([int]0); $writer.Write([int]0); $writer.Write([int]0)
    for ($y = $size - 1; $y -ge 0; $y--) {
        for ($x = 0; $x -lt $size; $x++) {
            $c = $bitmap.GetPixel($x, $y)
            $writer.Write([byte]$c.B); $writer.Write([byte]$c.G); $writer.Write([byte]$c.R); $writer.Write([byte]$c.A)
        }
    }
    $maskStride = [int]([Math]::Ceiling($size / 32.0) * 4)
    $writer.Write((New-Object byte[] ($maskStride * $size)))
    $writer.Flush()
    return , $stream.ToArray()
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$frames = foreach ($size in $sizes) {
    $bitmap = New-IconBitmap $size
    # 256 px is the one size conventionally stored as PNG.
    $bytes = if ($size -eq 256) { Get-PngBytes $bitmap } else { Get-DibBytes $bitmap }
    if ($size -eq 64) { $bitmap.Save((Join-Path $OutDir 'icon-64.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $bitmap.Dispose()
    , @{ Size = $size; Bytes = $bytes }
}

$file = New-Object System.IO.MemoryStream
$out = New-Object System.IO.BinaryWriter $file
$out.Write([int16]0); $out.Write([int16]1); $out.Write([int16]$frames.Count)
$offset = 6 + 16 * $frames.Count
foreach ($frame in $frames) {
    $dimension = if ($frame.Size -eq 256) { 0 } else { $frame.Size }
    $out.Write([byte]$dimension); $out.Write([byte]$dimension); $out.Write([byte]0); $out.Write([byte]0)
    $out.Write([int16]1); $out.Write([int16]32)
    $out.Write([int]$frame.Bytes.Length); $out.Write([int]$offset)
    $offset += $frame.Bytes.Length
}
foreach ($frame in $frames) { $out.Write([byte[]]$frame.Bytes) }
$out.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $OutDir 'icon.ico'), $file.ToArray())
