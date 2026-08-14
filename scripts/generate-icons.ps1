# 生成应用图标：icon.ico + 各尺寸 PNG（32/128/256）
# 用法：pwsh -File scripts/generate-icons.ps1
$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

$OutDir = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\src-tauri\icons'))
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# ---- 绘制主图标（圆角渐变方块 + DSH 字样 + 气泡尾巴） ----
function New-AppIcon([int]$size) {
    $bmp = [System.Drawing.Bitmap]::new($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    $scale = $size / 256.0
    $rect = [System.Drawing.RectangleF]::new(8 * $scale, 8 * $scale, 240 * $scale, 240 * $scale)
    $r = 52 * $scale

    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $path.AddArc($rect.X, $rect.Y, 2 * $r, 2 * $r, 180, 90)
    $path.AddArc($rect.Right - 2 * $r, $rect.Y, 2 * $r, 2 * $r, 270, 90)
    $path.AddArc($rect.Right - 2 * $r, $rect.Bottom - 2 * $r, 2 * $r, 2 * $r, 0, 90)
    $path.AddArc($rect.X, $rect.Bottom - 2 * $r, 2 * $r, 2 * $r, 90, 90)
    $path.CloseFigure()

    $brush = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
        $rect,
        [System.Drawing.Color]::FromArgb(255, 82, 145, 255),
        [System.Drawing.Color]::FromArgb(255, 16, 185, 129),
        45.0)
    $g.FillPath($brush, $path)

    # 左下角聊天气泡尾巴
    $tail = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(235, 82, 145, 255))
    $tailPts = [System.Drawing.PointF[]]@(
        [System.Drawing.PointF]::new($rect.X + 34 * $scale, $rect.Bottom - 46 * $scale),
        [System.Drawing.PointF]::new($rect.X + 74 * $scale, $rect.Bottom - 40 * $scale),
        [System.Drawing.PointF]::new($rect.X + 44 * $scale, $rect.Bottom - 78 * $scale)
    )
    $g.FillPolygon($tail, $tailPts)
    $tail.Dispose()

    # DSH 文字
    $fontSize = [single](92 * $scale)
    $font = [System.Drawing.Font]::new('Segoe UI', $fontSize, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
    $sf = [System.Drawing.StringFormat]::new()
    $sf.Alignment = [System.Drawing.StringAlignment]::Center
    $sf.LineAlignment = [System.Drawing.StringAlignment]::Center
    $textBrush = [System.Drawing.SolidBrush]::new([System.Drawing.Color]::White)
    $g.DrawString('DSH', $font, $textBrush, $rect, $sf)

    $textBrush.Dispose(); $font.Dispose(); $sf.Dispose(); $brush.Dispose(); $path.Dispose()
    $g.Dispose()
    return $bmp
}

# ---- 由 PNG 字节生成 ICO（PNG 压缩条目，Vista+ 支持） ----
function New-IcoFile([string]$path, [int[]]$sizes) {
    $images = [System.Collections.Generic.List[object]]::new()
    foreach ($s in $sizes) {
        $bmp = New-AppIcon $s
        $ms = [System.IO.MemoryStream]::new()
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $images.Add(@{ size = $s; data = $ms.ToArray() })
        $ms.Dispose(); $bmp.Dispose()
    }

    $count = $images.Count
    $offset = 6 + 16 * $count
    $bytes = [System.Collections.Generic.List[byte]]::new()
    # ICONDIR
    $bytes.Add(0); $bytes.Add(0); $bytes.Add(1); $bytes.Add(0)
    $bytes.Add([byte]($count -band 0xFF)); $bytes.Add([byte](($count -shr 8) -band 0xFF))
    # ICONDIRENTRY
    foreach ($img in $images) {
        $s = $img.size
        $d = if ($s -ge 256) { 0 } else { $s }                    # 256 记为 0
        $bytes.Add([byte]$d)                                       # bWidth
        $bytes.Add([byte]$d)                                       # bHeight
        $bytes.Add(0)                                              # bColorCount
        $bytes.Add(0)                                              # bReserved
        $bytes.Add(1); $bytes.Add(0)                               # wPlanes = 1
        $bytes.Add(32); $bytes.Add(0)                              # wBitCount = 32
        $len = $img.data.Length
        $bytes.Add([byte]($len -band 0xFF)); $bytes.Add([byte](($len -shr 8) -band 0xFF))
        $bytes.Add([byte](($len -shr 16) -band 0xFF)); $bytes.Add([byte](($len -shr 24) -band 0xFF))
        $bytes.Add([byte]($offset -band 0xFF)); $bytes.Add([byte](($offset -shr 8) -band 0xFF))
        $bytes.Add([byte](($offset -shr 16) -band 0xFF)); $bytes.Add([byte](($offset -shr 24) -band 0xFF))
        $offset += $len
    }
    # 图像数据
    foreach ($img in $images) { $bytes.AddRange($img.data) }
    [System.IO.File]::WriteAllBytes($path, $bytes.ToArray())
}

# ---- 生成 icon.icns（PNG 压缩块：ic07=128 / ic08=256 / ic09=512 / ic10=1024） ----
function New-IcnsFile([string]$path) {
    $chunks = [System.Collections.Generic.List[object]]::new()
    foreach ($pair in @(@('ic07', 128), @('ic08', 256), @('ic09', 512), @('ic10', 1024))) {
        $bmp = New-AppIcon $pair[1]
        $ms = [System.IO.MemoryStream]::new()
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $chunks.Add(@{ type = $pair[0]; data = $ms.ToArray() })
        $ms.Dispose(); $bmp.Dispose()
    }
    $total = 8
    foreach ($c in $chunks) { $total += 8 + $c.data.Length }
    $bytes = [System.Collections.Generic.List[byte]]::new()
    # 文件头：'icns' + 总长度（大端）
    $bytes.AddRange([System.Text.Encoding]::ASCII.GetBytes('icns'))
    $bytes.Add([byte](($total -shr 24) -band 0xFF)); $bytes.Add([byte](($total -shr 16) -band 0xFF))
    $bytes.Add([byte](($total -shr 8) -band 0xFF));  $bytes.Add([byte]($total -band 0xFF))
    foreach ($c in $chunks) {
        $len = 8 + $c.data.Length
        $bytes.AddRange([System.Text.Encoding]::ASCII.GetBytes($c.type))
        $bytes.Add([byte](($len -shr 24) -band 0xFF)); $bytes.Add([byte](($len -shr 16) -band 0xFF))
        $bytes.Add([byte](($len -shr 8) -band 0xFF));  $bytes.Add([byte]($len -band 0xFF))
        $bytes.AddRange($c.data)
    }
    [System.IO.File]::WriteAllBytes($path, $bytes.ToArray())
}

# ---- 输出 ----
Write-Host "生成图标到 $OutDir ..."
New-IcoFile (Join-Path $OutDir 'icon.ico') @(16, 24, 32, 48, 64, 128, 256)
New-IcnsFile (Join-Path $OutDir 'icon.icns')
foreach ($pair in @(@(32, '32x32.png'), @(128, '128x128.png'), @(256, '128x128@2x.png'))) {
    $bmp = New-AppIcon $pair[0]
    $bmp.Save((Join-Path $OutDir $pair[1]), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}
Write-Host '完成。'
Get-ChildItem $OutDir | Select-Object Name, Length | Format-Table -AutoSize
