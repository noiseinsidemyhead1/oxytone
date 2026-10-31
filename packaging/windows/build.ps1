# Fabrique l'installateur Windows : dist\ytmr-setup-<version>.exe
#
# En clair : compile ytmr en release, puis Inno Setup emballe ytmr.exe dans
# un installateur. Prérequis une fois : `scoop install inno-setup`.
#
# Usage (depuis la racine du dépôt, dans PowerShell) :
#   powershell -ExecutionPolicy Bypass -File packaging\windows\build.ps1
#   ... -TargetDir target\alt    # si ytmr est ouvert (target\release verrouillé)

param(
    [string]$TargetDir = "target"
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $root

# Version : celle du workspace (Cargo.toml).
$match = Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $match) { throw "version introuvable dans Cargo.toml" }
$version = $match.Matches[0].Groups[1].Value

Write-Host "Compilation de ytmr $version (release)..."
cargo build --release -p ytmr --target-dir $TargetDir
if ($LASTEXITCODE -ne 0) { throw "échec de la compilation" }

$exe = Resolve-Path (Join-Path $TargetDir "release\ytmr.exe")
$iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
if (-not $iscc) { throw "Inno Setup introuvable : scoop install inno-setup" }

Write-Host "Installateur..."
& $iscc "/DAppVersion=$version" "/DExePath=$exe" "packaging\windows\ytmr.iss"
if ($LASTEXITCODE -ne 0) { throw "échec d'Inno Setup" }

Write-Host "Prêt : dist\ytmr-setup-$version.exe"
