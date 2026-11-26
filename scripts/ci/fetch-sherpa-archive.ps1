# Download a sherpa-onnx prebuilt archive and verify it against the
# pinned SHA-256 (issue #144) — the PowerShell twin of
# fetch-sherpa-archive.sh for the Windows release legs. Prints the
# directory containing the verified archive; the caller exports it as
# SHERPA_ONNX_ARCHIVE_DIR so sherpa-onnx-sys uses the verified copy.
#
# Usage: scripts/ci/fetch-sherpa-archive.ps1 -Archive <filename>
param(
  [Parameter(Mandatory = $true)]
  [string]$Archive
)

$ErrorActionPreference = 'Stop'
$manifest = Join-Path $PSScriptRoot 'sherpa-archive-hashes.txt'

$line = Get-Content $manifest | Where-Object { $_ -match "\s+$([regex]::Escape($Archive))$" } | Select-Object -First 1
if (-not $line) {
  Write-Error "no pinned hash for '$Archive' in $manifest"
}
$expected = ($line -split '\s+')[0]

if ($Archive -notmatch 'sherpa-onnx-v([0-9.]+)') {
  Write-Error "cannot parse a sherpa-onnx version from '$Archive'"
}
$version = $Matches[1]
$base = "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$version"
$out = Join-Path $env:RUNNER_TEMP 'sherpa-archive-verified'
New-Item -ItemType Directory -Force -Path $out | Out-Null
$dest = Join-Path $out $Archive

$attempts = 0
do {
  $attempts++
  try {
    Invoke-WebRequest -Uri "$base/$Archive" -OutFile $dest
  } catch {
    if ($attempts -ge 3) { throw }
    Start-Sleep -Seconds (5 * $attempts)
  }
} while (-not (Test-Path $dest) -and $attempts -lt 3)

$actual = (Get-FileHash -Algorithm SHA256 -Path $dest).Hash.ToLower()
if ($actual -ne $expected.ToLower()) {
  Write-Error "sha256 mismatch for $Archive`: expected $expected, got $actual"
}
Write-Host "verified $Archive (sha256 $actual)"
Write-Output $out
