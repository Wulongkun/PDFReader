# Generate PDFReader app icons (blue rounded square + three white lines).
# Usage: powershell -ExecutionPolicy Bypass -File scripts/gen-icons.ps1
Add-Type -AssemblyName System.Drawing

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$iconsDir = Join-Path $root "src-tauri\icons"
New-Item -ItemType Directory -Force -Path $iconsDir | Out-Null

# Draw the logo (24-unit coordinate system scaled to `size` pixels).
function New-LogoBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $f = [float]($size / 24.0)
    $x = [float](2 * $f); $y = [float](2 * $f)
    $w = [float](20 * $f); $h = [float](20 * $f); $r = [float](5 * $f)
    $d = [float](2 * $r)

    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($x, $y, $d, $d, [float]180, [float]90)
    $path.AddArc($x + $w - $d, $y, $d, $d, [float]270, [float]90)
    $path.AddArc($x + $w - $d, $y + $h - $d, $d, $d, [float]0, [float]90)
    $path.AddArc($x, $y + $h - $d, $d, $d, [float]90, [float]90)
    $path.CloseFigure()

    $blue = [System.Drawing.Color]::FromArgb(59, 130, 246)
    $brush = New-Object System.Drawing.SolidBrush($blue)
    $g.FillPath($brush, $path)

    $pen = New-Object System.Drawing.Pen([System.Drawing.Color]::White, [float](2 * $f))
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $g.DrawLine($pen, [float](7.5 * $f), [float](9 * $f), [float](16.5 * $f), [float](9 * $f))
    $g.DrawLine($pen, [float](7.5 * $f), [float](12.5 * $f), [float](16.5 * $f), [float](12.5 * $f))
    $g.DrawLine($pen, [float](7.5 * $f), [float](16 * $f), [float](13 * $f), [float](16 * $f))

    $pen.Dispose(); $brush.Dispose(); $path.Dispose(); $g.Dispose()
    return $bmp
}

# Bitmap -> PNG byte array (for ICO embedding).
function Get-PngBytes($bmp) {
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $bytes = $ms.ToArray()
    $ms.Dispose()
    return ,$bytes
}

# Write a single PNG file.
function Save-Png($bmp, [string]$path) {
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
}

# Assemble a multi-size ICO from PNGs (PNG-embedded, Vista+).
function Build-Ico([int[]]$sizes) {
    $pngs = @{}
    foreach ($s in $sizes) {
        $bmp = New-LogoBitmap $s
        $pngs[$s] = Get-PngBytes $bmp
        $bmp.Dispose()
    }
    $count = $sizes.Count
    $offset = 6 + 16 * $count
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)
    $bw.Write([UInt16]0)
    $bw.Write([UInt16]1)
    $bw.Write([UInt16]$count)
    foreach ($s in $sizes) {
        $wb = [byte]($(if ($s -ge 256) { 0 } else { $s }))
        $bw.Write([byte]$wb)
        $bw.Write([byte]$wb)
        $bw.Write([byte]0)
        $bw.Write([byte]0)
        $bw.Write([UInt16]1)
        $bw.Write([UInt16]32)
        $bw.Write([UInt32]$pngs[$s].Length)
        $bw.Write([UInt32]$offset)
        $offset += $pngs[$s].Length
    }
    foreach ($s in $sizes) { $bw.Write($pngs[$s]) }
    $bw.Flush()
    $bytes = $ms.ToArray()
    $bw.Dispose(); $ms.Dispose()
    return ,$bytes
}

$pngJobs = @(
    @{ size = 32;   file = "32x32.png" },
    @{ size = 128;  file = "128x128.png" },
    @{ size = 256;  file = "128x128@2x.png" },
    @{ size = 512;  file = "icon.png" }
)
foreach ($job in $pngJobs) {
    $bmp = New-LogoBitmap $job.size
    $out = Join-Path $iconsDir $job.file
    Save-Png $bmp $out
    $bmp.Dispose()
    Write-Host "Wrote $out"
}

$icoBytes = Build-Ico @(16, 24, 32, 48, 64, 128, 256)
$icoPath = Join-Path $iconsDir "icon.ico"
[System.IO.File]::WriteAllBytes($icoPath, $icoBytes)
Write-Host "Wrote $icoPath"
Write-Host "Done."
