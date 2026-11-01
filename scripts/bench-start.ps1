# How long whir takes to show a file: repeated launches of a release build, timed by its
# own startup trace (WHIR_TRACE). Files are in the OS cache, as in the README's table: this
# compares builds, it doesn't measure a first launch after a reboot.
#
#   scripts\bench-start.ps1 [-Runs 20] [-Against other\whir.exe]
#
# The GPU driver alone moves a launch by 15 ms from one run to the next, far more than the
# UI costs, and nothing whir does changes it. So "after GPU" (from the device being ready to
# the content being shown) is the number to compare, and -Against alternates launches of two
# builds so that whatever else the PC is doing lands on both.
#
# It works on copies of the builds in a temp folder, with %LOCALAPPDATA%, the notes, the
# screenshots and the timesheet all pointed there, and no capture dock. A build must know
# WHIR_NO_DOCK: one that doesn't would start a dock from the temp folder, which points the
# start-with-Windows entry there. It won't run while a whir window is open: the launches
# would become tabs in that window.
param(
    [int]$Runs = 20,
    [string]$Exe = "",
    [string]$Against = "",
    [string]$Jpeg = "",
    [string]$Pdf = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
if (-not $Exe) { $Exe = Join-Path $root "target\release\whir.exe" }
if (-not (Test-Path $Exe)) { throw "No build at $Exe (cargo build --release -p whir)" }
if ($Against -and -not (Test-Path $Against)) { throw "No build at $Against" }

$open = Get-CimInstance Win32_Process -Filter "Name = 'whir.exe'" |
    Where-Object { $_.CommandLine -notmatch '--dock' }
if ($open) { throw "A whir window is open (pid $($open.ProcessId -join ', ')). Close it first." }

$work = Join-Path ([IO.Path]::GetTempPath()) "whir-bench"
if (Test-Path $work) { Remove-Item $work -Recurse -Force }
$data = Join-Path $work "appdata"
foreach ($d in $data, "$work\notes", "$work\shots", "$work\time", "$work\files") {
    New-Item -ItemType Directory -Force $d | Out-Null
}

function Install-Build([string]$Name, [string]$From) {
    $dir = Join-Path $work $Name
    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item $From (Join-Path $dir "whir.exe")
    Copy-Item (Join-Path $root "vendor\pdfium\bin\pdfium.dll") $dir
    $dir
}
$builds = [ordered]@{ this = Install-Build "this" $Exe }
if ($Against) { $builds["against"] = Install-Build "against" $Against }

function New-Jpeg([string]$Path) {
    Add-Type -AssemblyName System.Drawing
    $bitmap = [Drawing.Bitmap]::new(6000, 4000)
    $g = [Drawing.Graphics]::FromImage($bitmap)
    $whole = [Drawing.Rectangle]::new(0, 0, 6000, 4000)
    $sky = [Drawing.Drawing2D.LinearGradientBrush]::new(
        $whole, [Drawing.Color]::SteelBlue, [Drawing.Color]::Bisque, 90.0)
    $g.FillRectangle($sky, $whole)
    # Enough detail that the file is a realistic size for its pixels.
    $random = [Random]::new(7)
    for ($i = 0; $i -lt 40000; $i++) {
        $color = [Drawing.Color]::FromArgb(90, $random.Next(256), $random.Next(256), $random.Next(256))
        $brush = [Drawing.SolidBrush]::new($color)
        $g.FillEllipse($brush, $random.Next(6000), $random.Next(4000), $random.Next(4, 90), $random.Next(4, 90))
        $brush.Dispose()
    }
    $codec = [Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() |
        Where-Object { $_.MimeType -eq 'image/jpeg' }
    $quality = [Drawing.Imaging.EncoderParameters]::new(1)
    $quality.Param[0] = [Drawing.Imaging.EncoderParameter]::new([Drawing.Imaging.Encoder]::Quality, 90L)
    $bitmap.Save($Path, $codec, $quality)
    $g.Dispose(); $bitmap.Dispose()
}

function New-Pdf([string]$Path, [int]$Pages) {
    $objects = [Collections.Generic.List[string]]::new()
    $kids = (0..($Pages - 1) | ForEach-Object { "$(4 + 2 * $_) 0 R" }) -join ' '
    $objects.Add("<< /Type /Catalog /Pages 2 0 R >>")
    $objects.Add("<< /Type /Pages /Kids [$kids] /Count $Pages >>")
    $objects.Add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    $line = "The quick brown fox jumps over the lazy dog while the rack price holds steady."
    for ($k = 0; $k -lt $Pages; $k++) {
        $body = (1..44 | ForEach-Object { "($line) Tj T*" }) -join "`n"
        $content = "BT /F1 11 Tf 14 TL 60 730 Td`n$body`nET`n"
        $objects.Add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents $(5 + 2 * $k) 0 R " +
            "/Resources << /Font << /F1 3 0 R >> >> >>")
        $objects.Add("<< /Length $($content.Length) >>`nstream`n${content}endstream")
    }
    $text = [Text.StringBuilder]::new("%PDF-1.4`n")
    $offsets = [Collections.Generic.List[int]]::new()
    for ($i = 0; $i -lt $objects.Count; $i++) {
        $offsets.Add($text.Length)
        [void]$text.Append("$($i + 1) 0 obj`n$($objects[$i])`nendobj`n")
    }
    $xref = $text.Length
    [void]$text.Append("xref`n0 $($objects.Count + 1)`n0000000000 65535 f `n")
    foreach ($o in $offsets) { [void]$text.Append(('{0:D10} 00000 n ' -f $o) + "`n") }
    [void]$text.Append("trailer`n<< /Size $($objects.Count + 1) /Root 1 0 R >>`nstartxref`n$xref`n%%EOF`n")
    [IO.File]::WriteAllText($Path, $text.ToString(), [Text.Encoding]::ASCII)
}

if (-not $Jpeg) { $Jpeg = "$work\files\photo.jpg"; New-Jpeg $Jpeg }
if (-not $Pdf) { $Pdf = "$work\files\report.pdf"; New-Pdf $Pdf 66 }

$saved = @{}
$environment = @{
    WHIR_TRACE = "1"; WHIR_NO_DOCK = "1"; LOCALAPPDATA = $data
    WHIR_NOTES_DIR = "$work\notes"; WHIR_CAPTURES_DIR = "$work\shots"; WHIR_TIME_DIR = "$work\time"
}
foreach ($k in $environment.Keys) {
    $saved[$k] = [Environment]::GetEnvironmentVariable($k)
    [Environment]::SetEnvironmentVariable($k, $environment[$k])
}

$trace = Join-Path $data "whir\trace.log"
$cases = [ordered]@{ bare = @(); jpeg = @($Jpeg); pdf = @($Pdf) }

function Median([double[]]$Values) {
    if (-not $Values) { return [double]::NaN }
    $sorted = @($Values | Sort-Object)
    $sorted[[int][math]::Floor($sorted.Count / 2)]
}

# One launch: the time of each mark, and its private memory.
function Measure-Launch([string]$Dir, [string[]]$Files) {
    Remove-Item $trace -ErrorAction SilentlyContinue
    $start = @{ FilePath = "$Dir\whir.exe"; PassThru = $true; WorkingDirectory = $Dir }
    if ($Files.Count) { $start.ArgumentList = $Files | ForEach-Object { "`"$_`"" } }
    $process = Start-Process @start
    $deadline = [DateTime]::Now.AddSeconds(10)
    $lines = @()
    while ([DateTime]::Now -lt $deadline) {
        if (Test-Path $trace) {
            $lines = @(Get-Content $trace -ErrorAction SilentlyContinue)
            if ($lines -match 'shown') { break }
        }
        Start-Sleep -Milliseconds 20
    }
    Start-Sleep -Milliseconds 400
    $process.Refresh()
    $private = $process.PrivateMemorySize64
    [void]$process.CloseMainWindow()
    if (-not $process.WaitForExit(3000)) { Stop-Process -Id $process.Id -Force }
    if (-not ($lines -match 'shown')) { throw "$Dir never showed anything" }
    $marks = @{ memory = $private / 1MB }
    foreach ($line in $lines) {
        if ($line -match '\s([\d.]+) ms  (.+)$') {
            $name = $Matches[2] -replace '^shown.*', 'shown'
            if (-not $marks.ContainsKey($name)) { $marks[$name] = [double]$Matches[1] }
        }
    }
    $marks
}

try {
    foreach ($case in $cases.Keys) {
        foreach ($dir in $builds.Values) { [void](Measure-Launch $dir $cases[$case]) } # warms the OS cache
        $samples = @{}
        foreach ($name in $builds.Keys) {
            $samples[$name] = @{
                shown = [Collections.Generic.List[double]]::new()
                gpu = [Collections.Generic.List[double]]::new()
                after = [Collections.Generic.List[double]]::new()
                first = [Collections.Generic.List[double]]::new()
                memory = [Collections.Generic.List[double]]::new()
            }
        }
        for ($run = 0; $run -lt $Runs; $run++) {
            # Whichever goes first shouldn't always be the same one.
            $order = @($builds.Keys)
            if ($run % 2) { [array]::Reverse($order) }
            foreach ($name in $order) {
                $m = Measure-Launch $builds[$name] $cases[$case]
                $s = $samples[$name]
                $s.shown.Add($m['shown'])
                $s.gpu.Add($m['gpu device ready'])
                $s.after.Add($m['shown'] - $m['gpu device ready'])
                if ($m.ContainsKey('presented')) { $s.first.Add($m['presented'] - $m['first frame']) }
                $s.memory.Add($m['memory'])
            }
        }
        ""
        "$case"
        "  {0,-10} {1,10} {2,10} {3,10} {4,12} {5,10}" -f "", "shown", "GPU ready", "after GPU", "first frame", "memory"
        foreach ($name in $builds.Keys) {
            $s = $samples[$name]
            "  {0,-10} {1,10:N1} {2,10:N1} {3,10:N1} {4,12:N1} {5,10:N1}" -f $name,
                (Median $s.shown.ToArray()), (Median $s.gpu.ToArray()), (Median $s.after.ToArray()),
                (Median $s.first.ToArray()), (Median $s.memory.ToArray())
        }
        if ($Against) {
            $change = (Median $samples.this.after.ToArray()) - (Median $samples.against.after.ToArray())
            "  this build, after GPU: {0:+0.0;-0.0;0.0} ms" -f $change
        }
    }
} finally {
    foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) }
}
""
"whir.exe: this {0:N2} MB{1}, median of $Runs launches each (ms since process creation)" -f
    ((Get-Item $Exe).Length / 1MB),
    $(if ($Against) { ", against {0:N2} MB" -f ((Get-Item $Against).Length / 1MB) } else { "" })
