# Gera assets\icon.ico (16 a 256 px) e assets\icon.png (256 px) a partir de um desenho vetorial.
# Uso: powershell -ExecutionPolicy Bypass -File assets\make-icon.ps1
Add-Type -AssemblyName System.Drawing
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path

function Draw-Icon([int]$n) {
    $bmp = New-Object System.Drawing.Bitmap $n, $n, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.PixelOffsetMode = 'HighQuality'
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $n / 256.0

    # Fundo: quadrado arredondado com degradê roxo (cor de destaque do app).
    $m = [Math]::Max(1, [Math]::Round(8 * $s)); $w = $n - 2 * $m; $r = 56 * $s
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($m, $m, 2 * $r, 2 * $r, 180, 90)
    $path.AddArc($m + $w - 2 * $r, $m, 2 * $r, 2 * $r, 270, 90)
    $path.AddArc($m + $w - 2 * $r, $m + $w - 2 * $r, 2 * $r, 2 * $r, 0, 90)
    $path.AddArc($m, $m + $w - 2 * $r, 2 * $r, 2 * $r, 90, 90)
    $path.CloseFigure()
    $bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.PointF 0, 0), (New-Object System.Drawing.PointF $n, $n), ([System.Drawing.Color]::FromArgb(255, 150, 120, 255)), ([System.Drawing.Color]::FromArgb(255, 84, 52, 214))
    $g.FillPath($bg, $path)

    $white = [System.Drawing.Color]::White
    # Triângulo de "play".
    $pts = [System.Drawing.PointF[]]@(
        (New-Object System.Drawing.PointF (78 * $s), (68 * $s)),
        (New-Object System.Drawing.PointF (78 * $s), (188 * $s)),
        (New-Object System.Drawing.PointF (176 * $s), (128 * $s))
    )
    $g.FillPolygon((New-Object System.Drawing.SolidBrush $white), $pts)

    # Ondas de transmissão (só onde há espaço para ficarem legíveis).
    if ($n -ge 32) {
        $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(235, 255, 255, 255)), ([float](14 * $s))
        $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
        $cx = 150 * $s; $cy = 106 * $s
        foreach ($rad in @(52, 86)) {
            $rr = $rad * $s
            $g.DrawArc($pen, [float]($cx - $rr), [float]($cy - $rr), [float](2 * $rr), [float](2 * $rr), -70, 50)
        }
    }
    $g.Dispose()
    return $bmp
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 128, 256
$pngs = @()
foreach ($n in $sizes) {
    $b = Draw-Icon $n
    $ms = New-Object System.IO.MemoryStream
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    if ($n -eq 256) { $b.Save((Join-Path $dir 'icon.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $b.Dispose()
    $pngs += , $ms.ToArray()
}

# ICO com imagens PNG (suportado desde o Windows Vista).
$out = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter $out
$bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $n = $sizes[$i]; $d = $pngs[$i]
    $bw.Write([byte]($n % 256)); $bw.Write([byte]($n % 256)); $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([UInt16]1); $bw.Write([UInt16]32); $bw.Write([UInt32]$d.Length); $bw.Write([UInt32]$offset)
    $offset += $d.Length
}
foreach ($d in $pngs) { $bw.Write($d) }
$bw.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $dir 'icon.ico'), $out.ToArray())
"icon.ico: $($out.Length) bytes"
