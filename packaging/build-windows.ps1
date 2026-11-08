# Fichier: packaging/build-windows.ps1
# Pourquoi: build release + installeur NSIS (target\packager\ControlAnything_<ver>_x64-setup.exe)
# Usage: powershell -ExecutionPolicy Bypass -File packaging\build-windows.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Get-Command cargo-packager -ErrorAction SilentlyContinue)) {
    cargo install cargo-packager --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo install cargo-packager a echoue" }
}

cargo build --release -p hub-app -p hub-daemon
if ($LASTEXITCODE -ne 0) { throw "cargo build a echoue" }

# NSIS est telecharge automatiquement par cargo-packager au premier lancement.
cargo packager --release -c packaging\Packager.toml -f nsis
if ($LASTEXITCODE -ne 0) { throw "cargo packager a echoue" }

Get-ChildItem "$root\target\packager\*-setup.exe" |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1 |
    ForEach-Object { "Installeur : $($_.FullName) ($([math]::Round($_.Length / 1MB, 1)) Mo)" }
