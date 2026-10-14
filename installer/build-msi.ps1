# Builds a release exe and packages it as an MSI in target\wix.
#
# Requires the WiX Toolset v5 .NET tool:
#   dotnet tool install --global wix --version 5.0.2
#
# Optional: bake OAuth client IDs into the build (see README) by setting
# OUTLOOK_RS_MS_CLIENT_ID / OUTLOOK_RS_GOOGLE_CLIENT_ID / OUTLOOK_RS_GOOGLE_CLIENT_SECRET first.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    $env:PATH += ";$env:USERPROFILE\.dotnet\tools"
}
if (-not (Get-Command wix -ErrorAction SilentlyContinue)) {
    throw 'WiX not found. Install it with: dotnet tool install --global wix --version 5.0.2'
}

# MSI versions must be numeric (major.minor.build), so drop any -pre/+build suffix.
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$msiVersion = ($version -split '[-+]')[0]

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$out = Join-Path $root "target\wix\OutlookRS-$msiVersion-x64.msi"
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null
wix build installer\outlook-rs.wxs -arch x64 `
    -d "Version=$msiVersion" `
    -d "ExePath=$root\target\release\outlook-rs.exe" `
    -o $out
if ($LASTEXITCODE -ne 0) { throw 'wix build failed' }

Write-Host "Built $out"
