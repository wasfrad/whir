# Generates crates/whir/whir.ico (PNG-compressed entries, 16-256 px) from the art beside it:
# whir.svg down to 32 px, and whir-small.svg, drawn for it, at 24 px and under. Run it after
# changing either; the .ico is committed. Edge draws the SVGs, which every Windows PC has.
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot -Parent
$art = Join-Path $root "crates\whir"
$out = Join-Path $art "whir.ico"
$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)

$edge = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe",
    "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $edge) { throw "Microsoft Edge not found: it draws the SVGs" }

$uri = {
    param($name)
    $bytes = [System.IO.File]::ReadAllBytes((Join-Path $art $name))
    "data:image/svg+xml;base64," + [Convert]::ToBase64String($bytes)
}
$large = & $uri "whir.svg"
$small = & $uri "whir-small.svg"

# Every size on one transparent strip, drawn at its exact pixel size, then cut apart.
$gap = 8
$x = 0
$places = @{}
$imgs = foreach ($s in $sizes) {
    $places[$s] = $x
    $src = if ($s -le 24) { $small } else { $large }
    "<img src='$src' style='position:absolute;left:${x}px;top:0;width:${s}px;height:${s}px'>"
    $x += $s + $gap
}
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("whir-icon-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
    $html = Join-Path $tmp "strip.html"
    $png = Join-Path $tmp "strip.png"
    Set-Content -Path $html -Encoding utf8 -Value "<!doctype html><style>html,body{margin:0;background:transparent}</style>$($imgs -join '')"
    $flags = @("--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
        "--default-background-color=00000000", "--user-data-dir=`"$tmp\profile`"",
        "--window-size=$($x + $gap),320", "--screenshot=`"$png`"", "`"file:///$($html -replace '\\', '/')`"")
    Start-Process -FilePath $edge -ArgumentList $flags -Wait -WindowStyle Hidden
    if (-not (Test-Path $png)) { throw "Edge didn't draw the icon" }

    $strip = [System.Drawing.Bitmap]::FromFile($png)
    if ($strip.GetPixel($places[256] + 255, 255).A -ne 0) { throw "The strip isn't transparent" }
    $pngs = foreach ($s in $sizes) {
        $rect = New-Object System.Drawing.Rectangle $places[$s], 0, $s, $s
        $bmp = $strip.Clone($rect, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $ms = New-Object System.IO.MemoryStream
        $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        , $ms.ToArray()
    }
    $strip.Dispose()
} finally {
    Start-Sleep -Milliseconds 300
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$fs = [System.IO.File]::Create($out)
$bw = New-Object System.IO.BinaryWriter $fs
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $len = $pngs[$i].Length
    $bw.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
    $bw.Write([byte]($(if ($s -ge 256) { 0 } else { $s })))
    $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([uint16]1); $bw.Write([uint16]32)
    $bw.Write([uint32]$len); $bw.Write([uint32]$offset)
    $offset += $len
}
foreach ($p in $pngs) { $bw.Write($p) }
$bw.Close()
"wrote $out ($((Get-Item $out).Length) bytes)"
