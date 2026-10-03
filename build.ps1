<#
  [UNTITLED] TOOLS - builder
  Builds the app from source. If Rust or the C++ build tools are missing it offers to
  install them (official Rust installer from static.rust-lang.org, hash-checked; C++ Build
  Tools via winget). Nothing is installed without your OK unless you pass -Yes.

  Usage:   powershell -ExecutionPolicy Bypass -File build.ps1 [-NoRun] [-Yes]
#>
[CmdletBinding()]
param(
    [switch]$NoRun,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root

function Say($m)  { Write-Host "[UNTITLED] TOOLS builder: $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "[UNTITLED] TOOLS builder: $m" -ForegroundColor Yellow }
function Ask($q)  {
    if ($Yes) { return $true }
    $a = Read-Host "$q [y/N]"
    return ($a -match '^(y|yes)$')
}

function Find-Cargo {
    $c = Get-Command cargo -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    $p = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path $p) {
        $env:Path = (Split-Path $p) + ';' + $env:Path
        return $p
    }
    return $null
}

function Test-MsvcTools {
    $pf = ${env:ProgramFiles(x86)}
    if (-not $pf) { return $false }
    $vw = Join-Path $pf 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vw)) { return $false }
    $p = & $vw -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    return [bool]$p
}

try {
    if (-not (Test-Path (Join-Path $Root 'Cargo.toml'))) { throw "Cargo.toml not found next to build.ps1" }

    # 1) C++ linker (needed by Rust on Windows)
    if (-not (Test-MsvcTools)) {
        Warn "Microsoft C++ Build Tools were not found (Rust needs them to link programs)."
        $wg = Get-Command winget -ErrorAction SilentlyContinue
        if ($wg -and (Ask "Install them now with winget? (large download, may ask for admin rights)")) {
            & winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-package-agreements --accept-source-agreements --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
            if (-not (Test-MsvcTools)) { throw "C++ Build Tools still not detected. Reboot and re-run, or install from https://visualstudio.microsoft.com/visual-cpp-build-tools/ (tick 'Desktop development with C++')." }
        } else {
            throw "Install 'Desktop development with C++' from https://visualstudio.microsoft.com/visual-cpp-build-tools/ and run this script again."
        }
    }

    # 2) Rust toolchain
    $cargo = Find-Cargo
    if (-not $cargo) {
        Say "Rust not found."
        if (-not (Ask "Download and install Rust (rustup) for your user account?")) { throw "Rust is required. Install from https://rustup.rs and re-run." }
        $url = 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe'
        $tmp = Join-Path $env:TEMP 'rustup-init.exe'
        Say "Downloading rustup-init.exe ..."
        Invoke-WebRequest -Uri $url -OutFile $tmp -UseBasicParsing
        $expected = $null
        try {
            $c = (Invoke-WebRequest -Uri "$url.sha256" -UseBasicParsing).Content
            if ($c -is [byte[]]) { $c = [Text.Encoding]::ASCII.GetString($c) }
            $expected = (($c.Trim() -split '\s+')[0]).ToLower()
        } catch {}
        $actual = (Get-FileHash $tmp -Algorithm SHA256).Hash.ToLower()
        if ($expected) {
            if ($expected -ne $actual) { Remove-Item $tmp -Force; throw "rustup-init.exe hash mismatch - aborting." }
            Say "Installer hash verified."
        } else {
            Warn "Could not fetch the published hash for rustup-init.exe."
            if (-not (Ask "Continue without hash verification?")) { Remove-Item $tmp -Force; throw "Aborted." }
        }
        $ErrorActionPreference = 'Continue'
        & $tmp -y --profile minimal --default-toolchain stable
        $code = $LASTEXITCODE
        $ErrorActionPreference = 'Stop'
        Remove-Item $tmp -Force -ErrorAction SilentlyContinue
        if ($code -ne 0) { throw "rustup failed (exit $code)." }
        $cargo = Find-Cargo
        if (-not $cargo) { throw "cargo not found after install. Open a new terminal and re-run." }
    }

    # 3) Build
    Say "Building (first build downloads crates; a few minutes)..."
    $ErrorActionPreference = 'Continue'
    & $cargo build --release
    $code = $LASTEXITCODE
    $ErrorActionPreference = 'Stop'
    if ($code -ne 0) { throw "cargo build failed (exit $code). Copy the error text and send it back for a fix." }

    New-Item -ItemType Directory -Force -Path (Join-Path $Root 'dist') | Out-Null
    Copy-Item (Join-Path $Root 'target\release\untitled-tools.exe') (Join-Path $Root 'dist\untitled-tools.exe') -Force
    Copy-Item (Join-Path $Root 'README.md') (Join-Path $Root 'dist\README.md') -Force
    Say "Done: dist\untitled-tools.exe"

    if (-not $NoRun) { Start-Process (Join-Path $Root 'dist\untitled-tools.exe') }
    exit 0
}
catch {
    Write-Host ""
    Write-Host "BUILD FAILED: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
