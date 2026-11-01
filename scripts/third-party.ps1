# Writes the license of every crate built into whir.exe to one file, which a
# binary release has to carry. Each distinct text appears once, under the crates
# it covers. Where a crate offers a choice, whir takes the permissive one, so a
# GPL text offered beside it is left out. Run by dist.ps1.
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
$unfiled = @()          # crates that ship no license file: name, terms and authors
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
        if ($spdx -match ' OR ') { $files = @($files | Where-Object { $_.Name -notmatch 'GPL' }) }
    }
    if ($files.Count -eq 0) {
        if ($spdx -notmatch '\bMIT\b') { throw "$crate ships no license file and isn't MIT: $spdx" }
        $manifest = if ($dir) { Get-Content (Join-Path $dir "Cargo.toml") -Raw } else { "" }
        $authors = if ($manifest -match '(?ms)^authors\s*=\s*\[(.*?)\]') {
            ([regex]::Matches($Matches[1], '"([^"<]+?)\s*(<[^"]*>)?"') | ForEach-Object { $_.Groups[1].Value }) -join ", "
        } else { "its authors" }
        $unfiled += "$crate ($spdx): Copyright (c) $authors"
        continue
    }
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
    [void]$sb.AppendLine("These crates ship no license file. whir uses them under MIT, which reads:")
    [void]$sb.AppendLine(("=" * 78))
    $mit = (Get-Content (Join-Path $root "LICENSE") -Raw) -replace "`r`n", "`n"
    [void]$sb.AppendLine("`n" + $mit.Substring($mit.IndexOf("Permission is hereby granted")).Trim() + "`n")
    foreach ($c in $unfiled) { [void]$sb.AppendLine($c) }
}
[System.IO.File]::WriteAllText($Out, $sb.ToString())
"third-party licenses: $($texts.Count) texts, $($unfiled.Count) crates without a file"
