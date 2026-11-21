# PeriWire bootstrap for Windows (PowerShell 5.1 or 7): download the release archive, check its SHA-256, unpack it, and let the program set itself up.
#   irm https://github.com/ImYourBoyRoy/periwire/releases/latest/download/install.ps1 | iex
# Options (environment): PERIWIRE_VERSION=0.1.2 (default: the latest release), PERIWIRE_INSTALL_DIR (default %LOCALAPPDATA%\PeriWire),
# PERIWIRE_RELEASE_BASE_URL (a folder with the archive and its .sha256: https, file://, or http on loopback).
# The real work (the user PATH) is `periwire self-install`; undo it with `periwire self-uninstall`, then delete the install folder.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$repo = 'ImYourBoyRoy/periwire'
function Fail($m) { Write-Error "install.ps1: $m"; exit 1 }

if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { Fail "no PeriWire download for $($env:PROCESSOR_ARCHITECTURE) yet" }
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$version = $env:PERIWIRE_VERSION
if (-not $version) {
  try {
    $req = [System.Net.WebRequest]::Create("https://github.com/$repo/releases/latest"); $req.AllowAutoRedirect = $false
    $loc = $req.GetResponse().Headers['Location']; $version = ($loc -split '/v')[-1]
  } catch { Fail 'could not find the latest release (is github.com reachable?)' }
}
$version = $version.TrimStart('v')
if ($version -notmatch '^[0-9A-Za-z.+-]+$') { Fail "odd version: $version" }

$base = if ($env:PERIWIRE_RELEASE_BASE_URL) { $env:PERIWIRE_RELEASE_BASE_URL.TrimEnd('/') } else { "https://github.com/$repo/releases/download/v$version" }
if ($base -notmatch '^(https://|file://|http://(127\.0\.0\.1|localhost|\[::1\]))') { Fail 'PERIWIRE_RELEASE_BASE_URL must be https://, file://, or http:// on this computer' }

$asset = "periwire-$version-windows-x86_64.zip"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("periwire-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
function Get-File($url, $dest) {
  if ($url -like 'file://*') { Copy-Item -LiteralPath ([Uri]$url).LocalPath -Destination $dest } else { Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing }
}
try {
  Write-Host "downloading $asset"
  Get-File "$base/$asset" (Join-Path $tmp $asset)
  $sum = Join-Path $tmp 'sum'
  try { Get-File "$base/$asset.sha256" $sum } catch { try { Get-File "$base/SHA256SUMS.txt" $sum } catch { Fail "the release has no checksum for $asset" } }
  $want = $null
  foreach ($line in Get-Content $sum) {
    $p = $line.Trim() -split '\s+'
    if ($p.Count -eq 1 -and $p[0].Length -eq 64) { $want = $p[0]; break }
    if ($p.Count -ge 2 -and $p[1].TrimStart('*') -eq $asset) { $want = $p[0]; break }
  }
  if (-not $want) { Fail "no checksum for $asset in the release" }
  $got = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $tmp $asset)).Hash
  if ($got -ne $want.ToUpper()) { Fail "the download does not match its checksum (expected $want, got $got)" }
  Write-Host 'checksum ok'

  $dest = if ($env:PERIWIRE_INSTALL_DIR) { $env:PERIWIRE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'PeriWire' }
  $x = Join-Path $tmp 'x'
  Expand-Archive -LiteralPath (Join-Path $tmp $asset) -DestinationPath $x
  $cli = Get-ChildItem -Path $x -Recurse -Filter 'periwire.exe' | Select-Object -First 1
  if (-not $cli) { Fail 'the archive has no periwire.exe' }
  New-Item -ItemType Directory -Force -Path $dest | Out-Null
  foreach ($name in 'periwire.exe', 'periwire-gui.exe') {
    $src = Join-Path $cli.DirectoryName $name
    if (-not (Test-Path $src)) { continue }
    $target = Join-Path $dest $name
    if (Test-Path $target) { Move-Item -Force $target "$target.old" -ErrorAction SilentlyContinue }  # a running program can be moved aside, not overwritten
    Copy-Item -Force $src $target
    Remove-Item -Force "$target.old" -ErrorAction SilentlyContinue
  }
  Write-Host "installed in $dest"
  & (Join-Path $dest 'periwire.exe') self-install --yes
  Write-Host 'Done. Open a new PowerShell or cmd window, then run: periwire doctor'
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
