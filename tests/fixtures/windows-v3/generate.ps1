param(
    [Parameter(Mandatory = $true)]
    [string]$Elfshaker,

    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
$Elfshaker = [System.IO.Path]::GetFullPath($Elfshaker)
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
$Root = Join-Path $env:TEMP "elfshaker-windows-writer-v3-fixture"
$Data = Join-Path $Root "data"
$Portable = Join-Path $Root "portable"
$Native = Join-Path $Root "native"

if (Test-Path $Root) {
    Remove-Item -Recurse -Force $Root
}
New-Item -ItemType Directory -Path $Data, $Portable, $Native | Out-Null
New-Item -ItemType Directory -Path (Join-Path $Portable "target-dir") | Out-Null
[System.IO.File]::WriteAllBytes(
    (Join-Path $Portable "target-file"),
    [System.Text.Encoding]::UTF8.GetBytes("windows fixture target`n")
)
[System.IO.File]::WriteAllBytes(
    (Join-Path $Portable "target-dir\child"),
    [System.Text.Encoding]::UTF8.GetBytes("windows fixture child`n")
)
[System.IO.File]::CreateSymbolicLink(
    (Join-Path $Portable "file-link"),
    "target-file"
) | Out-Null
[System.IO.Directory]::CreateSymbolicLink(
    (Join-Path $Portable "directory-link"),
    "target-dir"
) | Out-Null
[System.IO.File]::CreateSymbolicLink(
    (Join-Path $Portable "dangling-file"),
    "missing-file"
) | Out-Null
[System.IO.Directory]::CreateSymbolicLink(
    (Join-Path $Portable "dangling-directory"),
    "missing-directory"
) | Out-Null

[System.IO.File]::WriteAllBytes(
    (Join-Path $Native "sentinel"),
    [System.Text.Encoding]::UTF8.GetBytes("replacement`n")
)
[System.IO.File]::CreateSymbolicLink(
    (Join-Path $Native "absolute-link"),
    "C:\elfshaker-fixture-target"
) | Out-Null

Push-Location $Portable
try {
    & $Elfshaker --data-dir $Data store portable
    if ($LASTEXITCODE -ne 0) { throw "elfshaker store portable failed" }
} finally {
    Pop-Location
}

Push-Location $Native
try {
    & $Elfshaker --data-dir $Data store native
    if ($LASTEXITCODE -ne 0) { throw "elfshaker store native failed" }
    & $Elfshaker --data-dir $Data pack windows-writer-v3 --compression-level 1 --threads 1 --frames 1
    if ($LASTEXITCODE -ne 0) { throw "elfshaker pack failed" }
} finally {
    Pop-Location
}

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
Copy-Item (Join-Path $Data "packs\windows-writer-v3.pack") $OutputDirectory
Copy-Item (Join-Path $Data "packs\windows-writer-v3.pack.idx") $OutputDirectory
Get-FileHash -Algorithm SHA256 (Join-Path $OutputDirectory "windows-writer-v3.pack")
Get-FileHash -Algorithm SHA256 (Join-Path $OutputDirectory "windows-writer-v3.pack.idx")
