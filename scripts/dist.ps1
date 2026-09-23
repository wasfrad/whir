# Release build + portable folder: dist\whir\{whir.exe, pdfium.dll, licenses}.
# Copy that folder anywhere (no installer, no admin), then run:
#   whir.exe --register     (per-user "Open with" entries)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    cargo build --release -p whir
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

    $pdfium = Join-Path $root "vendor\pdfium\bin\pdfium.dll"
    if (-not (Test-Path $pdfium)) {
        & (Join-Path $PSScriptRoot "fetch-pdfium.ps1")
    }

    $out = Join-Path $root "dist\whir"
    if (Test-Path $out) { Remove-Item $out -Recurse -Force }
    New-Item -ItemType Directory -Force (Join-Path $out "licenses") | Out-Null
    Copy-Item (Join-Path $root "target\release\whir.exe") $out
    Copy-Item $pdfium $out
    Copy-Item (Join-Path $root "vendor\pdfium\LICENSE") (Join-Path $out "licenses\pdfium-LICENSE.txt")
    Copy-Item (Join-Path $root "vendor\pdfium\licenses\*") (Join-Path $out "licenses")

    Get-ChildItem $out -File | ForEach-Object { "{0,-12} {1,8:N0} KB" -f $_.Name, ($_.Length / 1KB) }
    "Portable build ready: $out"
} finally {
    Pop-Location
}
