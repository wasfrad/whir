# Downloads a pinned PDFium build (bblanchon/pdfium-binaries, BSD/Apache licensed)
# into vendor/pdfium and verifies its SHA-256. whir loads pdfium.dll lazily, only
# when the first PDF is opened.
param(
    [string]$Tag = "chromium/8066",
    [string]$Sha256 = "739a57d597d864297909cc40a2411eba728490c76a0fa25e3ea299c7f6b07020"
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$dest = Join-Path $root "vendor\pdfium"
$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$Tag/pdfium-win-x64.tgz"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("pdfium-" + [guid]::NewGuid() + ".tgz")

Write-Host "Downloading $url"
Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
$actual = (Get-FileHash -Algorithm SHA256 $tmp).Hash.ToLowerInvariant()
if ($actual -ne $Sha256.ToLowerInvariant()) {
    Remove-Item $tmp -Force
    throw "SHA-256 mismatch: expected $Sha256, got $actual"
}

if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
New-Item -ItemType Directory -Force $dest | Out-Null
tar -xzf $tmp -C $dest
Remove-Item $tmp -Force
Set-Content -Path (Join-Path $dest "VERSION.txt") -Value "$Tag`n$Sha256"
$dll = Join-Path $dest "bin\pdfium.dll"
if (-not (Test-Path $dll)) { throw "pdfium.dll not found in the archive" }
Write-Host ("OK: {0} ({1:N1} MB)" -f $dll, ((Get-Item $dll).Length / 1MB))
