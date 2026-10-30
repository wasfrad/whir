# Release build + portable folder: dist\whir\{whir.exe, pdfium.dll, README.md, licenses}.
# Copy that folder anywhere (no installer, no admin), then run:
#   whir.exe --register     (per-user "Open with" entries)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    # A running capture dock is whir.exe too, and would keep it from being replaced.
    # It is asked to close by its own exe (one old enough not to know the flag
    # can't be running a dock, and would take the flag for a file to open).
    Get-CimInstance Win32_Process -Filter "Name = 'whir.exe'" |
        Where-Object { $_.CommandLine -match '--dock' } |
        Select-Object -First 1 |
        ForEach-Object { & $_.ExecutablePath --dock-quit | Out-Null; Start-Sleep -Milliseconds 300 }
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
    # The user guide travels with the folder; .md opens in whir itself once registered.
    Copy-Item (Join-Path $root "docs\guide.md") (Join-Path $out "README.md")
    Copy-Item (Join-Path $root "vendor\pdfium\LICENSE") (Join-Path $out "licenses\pdfium-LICENSE.txt")
    Copy-Item (Join-Path $root "vendor\pdfium\licenses\*") (Join-Path $out "licenses")

    Get-ChildItem $out -File | ForEach-Object { "{0,-12} {1,8:N0} KB" -f $_.Name, ($_.Length / 1KB) }
    "Portable build ready: $out"
} finally {
    Pop-Location
}
