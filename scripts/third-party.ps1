# Writes the license of every crate built into whir.exe to one file, which a
# binary release has to carry. Each distinct text appears once, under the crates
# it covers. Run by dist.ps1.
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$registry = Join-Path $(if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME ".cargo" }) "registry\src"

Push-Location $root
try {
    $tree = cargo tree -p whir -e normal --target x86_64-pc-windows-msvc --prefix none --format "{p}|{l}" --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo tree failed" }
} finally {
    Pop-Location
}

$texts = [ordered]@{}   # normalized text -> names of the crates under it
$unfiled = @()          # crates that ship no license file: name and SPDX expression
foreach ($line in ($tree | ForEach-Object { $_ -replace ' \(\*\)$', '' } | Sort-Object -Unique)) {
    # Skip whir's own crates: "whir-core v0.1.0 (C:\...)|MIT".
    if ($line -notmatch '^(\S+) v(\S+)\|(.*)$') { continue }
    $crate = "$($Matches[1]) $($Matches[2])"
    $spdx = $Matches[3]
    $dir = Get-ChildItem $registry -Directory |
        ForEach-Object { Join-Path $_.FullName "$($Matches[1])-$($Matches[2])" } |
        Where-Object { Test-Path $_ } |
        Select-Object -First 1
    $files = @()
    if ($dir) {
        $files = @(Get-ChildItem $dir -Recurse -Depth 1 -File | Where-Object {
            $_.Name -match '^(licen[cs]e|copying|notice|unlicense)' -or $_.Directory.Name -match '^licen[cs]es$'
        })
    }
    if ($files.Count -eq 0) { $unfiled += "$crate ($spdx)"; continue }
    foreach ($f in $files) {
        $text = (Get-Content $f.FullName -Raw) -replace "`r`n", "`n"
        $text = $text.Trim()
        if (-not $texts.Contains($text)) { $texts[$text] = New-Object System.Collections.ArrayList }
        if (-not $texts[$text].Contains($crate)) { [void]$texts[$text].Add($crate) }
    }
}

$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine("The Rust crates built into whir.exe, and their licenses.`n")
foreach ($text in $texts.Keys) {
    [void]$sb.AppendLine(("=" * 78))
    [void]$sb.AppendLine(($texts[$text] -join ", "))
    [void]$sb.AppendLine(("=" * 78))
    [void]$sb.AppendLine("`n$text`n")
}
if ($unfiled.Count -gt 0) {
    [void]$sb.AppendLine(("=" * 78))
    [void]$sb.AppendLine("These crates ship no license file; their published license terms:")
    [void]$sb.AppendLine(("=" * 78))
    foreach ($c in $unfiled) { [void]$sb.AppendLine($c) }
}
[System.IO.File]::WriteAllText($Out, $sb.ToString())
"third-party licenses: $($texts.Count) texts, $($unfiled.Count) crates without a file"
