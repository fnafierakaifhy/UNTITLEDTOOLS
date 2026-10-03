<#
  [UNTITLED] TOOLS - MSI builder
  1. builds the app (build.ps1)
  2. prepares your artwork from src\assets (icon.png, banner.png, dialog.png, license) -> .ico/.bmp/.rtf
  3. makes sure the .NET SDK and the WiX tool are present (asks first; winget / NuGet, both hash/signature checked)
  4. writes dist\UNTITLED-TOOLS-<version>-x64.msi

  Usage:  powershell -ExecutionPolicy Bypass -File build-installer.ps1 [-Yes] [-SkipBuild]
#>
[CmdletBinding()]
param(
    [switch]$Yes,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $Root

$WixVersion = '5.0.2'

function Say($m)  { Write-Host "[UNTITLED] TOOLS installer: $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "[UNTITLED] TOOLS installer: $m" -ForegroundColor Yellow }
function Ask($q)  {
    if ($Yes) { return $true }
    $a = Read-Host "$q [y/N]"
    return ($a -match '^(y|yes)$')
}

Add-Type -AssemblyName System.Drawing

function New-Placeholder([string]$Dst, [int]$W, [int]$H, [string]$Text) {
    $bmp = New-Object System.Drawing.Bitmap $W, $H
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $rect = New-Object System.Drawing.Rectangle 0, 0, $W, $H
    $c1 = [System.Drawing.Color]::FromArgb(137, 217, 229)
    $c2 = [System.Drawing.Color]::FromArgb(96, 26, 210)
    $br = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, $c1, $c2, 45.0
    $g.FillRectangle($br, $rect)
    $font = New-Object System.Drawing.Font 'Segoe UI', ([single]([Math]::Max(10, $H / 4))), ([System.Drawing.FontStyle]::Bold)
    $g.DrawString($Text, $font, [System.Drawing.Brushes]::White, 6.0, ([single]($H / 4)))
    $g.Dispose(); $br.Dispose(); $font.Dispose()
    $bmp.Save($Dst, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

function Convert-ToBmp([string]$Src, [string]$Dst, [int]$W, [int]$H) {
    $img = [System.Drawing.Image]::FromFile($Src)
    try {
        $bmp = New-Object System.Drawing.Bitmap $W, $H, ([System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $g.Clear([System.Drawing.Color]::White)
        $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $g.DrawImage($img, 0, 0, $W, $H)
        $g.Dispose()
        $bmp.Save($Dst, [System.Drawing.Imaging.ImageFormat]::Bmp)
        $bmp.Dispose()
    } finally { $img.Dispose() }
}

function New-Ico([string]$PngPath, [string]$IcoPath) {
    $src = [System.Drawing.Image]::FromFile($PngPath)
    $sizes = @(16, 24, 32, 48, 64, 128, 256)
    $blobs = New-Object System.Collections.ArrayList
    try {
        foreach ($s in $sizes) {
            $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
            $g = [System.Drawing.Graphics]::FromImage($bmp)
            $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
            $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
            $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
            $g.Clear([System.Drawing.Color]::Transparent)
            $g.DrawImage($src, 0, 0, $s, $s)
            $g.Dispose()
            $ms = New-Object System.IO.MemoryStream
            $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
            [void]$blobs.Add($ms.ToArray())
            $bmp.Dispose(); $ms.Dispose()
        }
    } finally { $src.Dispose() }
    $fs = [System.IO.File]::Create($IcoPath)
    $bw = New-Object System.IO.BinaryWriter $fs
    try {
        $bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
        $offset = 6 + 16 * $sizes.Count
        for ($i = 0; $i -lt $sizes.Count; $i++) {
            $s = $sizes[$i]
            $d = 0
            if ($s -lt 256) { $d = $s }
            $len = $blobs[$i].Length
            $bw.Write([byte]$d); $bw.Write([byte]$d); $bw.Write([byte]0); $bw.Write([byte]0)
            $bw.Write([uint16]1); $bw.Write([uint16]32)
            $bw.Write([uint32]$len); $bw.Write([uint32]$offset)
            $offset += $len
        }
        foreach ($b in $blobs) { $bw.Write([byte[]]$b) }
    } finally { $bw.Close(); $fs.Close() }
}

function Test-Dotnet {
    $d = Get-Command dotnet -ErrorAction SilentlyContinue
    if (-not $d) { return $false }
    $s = & dotnet --list-sdks 2>$null
    return [bool]$s
}

try {
    # ---- version / names
    $cargoToml = Get-Content (Join-Path $Root 'Cargo.toml') -Raw
    if ($cargoToml -notmatch '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"') { throw 'Could not read the version from Cargo.toml' }
    $Version = $Matches[1]
    $Manufacturer = 'UNTITLED TOOLS'
    $companyFile = Join-Path $Root 'src\assets\company.txt'
    if (Test-Path $companyFile) {
        $first = (Get-Content $companyFile -TotalCount 1)
        if ($first -and $first.Trim()) { $Manufacturer = $first.Trim() }
    }

    # ---- 1) build the app
    if (-not $SkipBuild) {
        Say 'Building the app first...'
        $bargs = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', (Join-Path $Root 'build.ps1'), '-NoRun')
        if ($Yes) { $bargs += '-Yes' }
        & powershell @bargs
        if ($LASTEXITCODE -ne 0) { throw 'The app build failed - see the messages above.' }
    }
    $exe = Join-Path $Root 'dist\untitled-tools.exe'
    if (-not (Test-Path $exe)) { throw 'dist\untitled-tools.exe not found. Run build.ps1 first.' }
    $readme = Join-Path $Root 'README.md'

    # ---- 2) artwork
    $in = Join-Path $Root 'src\assets'
    $out = Join-Path $Root 'target\installer-assets'
    New-Item -ItemType Directory -Force -Path $out | Out-Null
    Say 'Preparing the installer artwork from src\assets ...'

    $iconPng = Join-Path $in 'icon.png'
    if (-not (Test-Path $iconPng)) { Warn 'src\assets\icon.png missing - using a placeholder.'; $iconPng = Join-Path $out 'icon-placeholder.png'; New-Placeholder $iconPng 512 512 '[U]' }
    New-Ico $iconPng (Join-Path $out 'icon.ico')

    $bannerPng = Join-Path $in 'banner.png'
    if (-not (Test-Path $bannerPng)) { Warn 'src\assets\banner.png missing - using a placeholder.'; $bannerPng = Join-Path $out 'banner-placeholder.png'; New-Placeholder $bannerPng 493 58 'UNTITLED TOOLS' }
    Convert-ToBmp $bannerPng (Join-Path $out 'banner.bmp') 493 58

    $dialogPng = Join-Path $in 'dialog.png'
    if (-not (Test-Path $dialogPng)) { Warn 'src\assets\dialog.png missing - using a placeholder.'; $dialogPng = Join-Path $out 'dialog-placeholder.png'; New-Placeholder $dialogPng 493 312 '[U]' }
    Convert-ToBmp $dialogPng (Join-Path $out 'dialog.bmp') 493 312

    $rtfOut = Join-Path $out 'license.rtf'
    if (Test-Path (Join-Path $in 'license.rtf')) {
        Copy-Item (Join-Path $in 'license.rtf') $rtfOut -Force
    } else {
        $txt = 'This program is provided "as is", without warranty.'
        if (Test-Path (Join-Path $in 'license.txt')) { $txt = Get-Content (Join-Path $in 'license.txt') -Raw }
        $txt = $txt -replace '\\', '\\' -replace '\{', '\{' -replace '\}', '\}'
        $txt = $txt -replace "`r?`n", '\par '
        $rtf = '{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\fs20 ' + $txt + '}'
        [System.IO.File]::WriteAllText($rtfOut, $rtf, [System.Text.Encoding]::ASCII)
    }

    # ---- 3) .NET SDK + WiX
    if (-not (Test-Dotnet)) {
        Warn 'The .NET SDK (needed to run the WiX installer-builder) was not found.'
        $wg = Get-Command winget -ErrorAction SilentlyContinue
        if ($wg -and (Ask 'Install the .NET 8 SDK with winget (Microsoft package, hash-verified)?')) {
            & winget install --id Microsoft.DotNet.SDK.8 -e --accept-package-agreements --accept-source-agreements
            $env:Path = $env:Path + ';' + (Join-Path $env:ProgramFiles 'dotnet')
            if (-not (Test-Dotnet)) { throw '.NET SDK still not detected. Open a new terminal and run this again.' }
        } else {
            throw 'Install the .NET 8 SDK from https://dotnet.microsoft.com/download and run this again.'
        }
    }
    $tools = Join-Path $env:USERPROFILE '.dotnet\tools'
    if ($env:Path -notlike "*$tools*") { $env:Path = "$tools;$env:Path" }

    $wixLine = (& dotnet tool list --global 2>$null) | Where-Object { $_ -match '^\s*wix\s' } | Select-Object -First 1
    if (-not $wixLine) {
        if (-not (Ask "Install the WiX tool $WixVersion from NuGet (signed packages) for your user account?")) { throw 'WiX is required to build the MSI.' }
        & dotnet tool install --global wix --version $WixVersion
        if ($LASTEXITCODE -ne 0) { throw 'Could not install the WiX tool.' }
        $wixLine = (& dotnet tool list --global 2>$null) | Where-Object { $_ -match '^\s*wix\s' } | Select-Object -First 1
    }
    $wixVer = $WixVersion
    if ($wixLine) {
        $parts = @($wixLine -split '\s+' | Where-Object { $_ })
        if ($parts.Count -ge 2) { $wixVer = $parts[1] }
    }
    $extLine = (& wix extension list --global 2>$null) | Where-Object { $_ -match 'WixToolset.UI.wixext' } | Select-Object -First 1
    if (-not $extLine) {
        & wix extension add --global "WixToolset.UI.wixext/$wixVer"
        if ($LASTEXITCODE -ne 0) { throw 'Could not add the WiX UI extension.' }
    }

    # ---- 4) build the MSI
    $msi = Join-Path $Root "dist\UNTITLED-TOOLS-$Version-x64.msi"
    Say "Building $msi ..."
    & wix build -arch x64 -sval -ext WixToolset.UI.wixext `
        -d "Version=$Version" -d "Manufacturer=$Manufacturer" -d "AssetsDir=$out" `
        -d "ExePath=$exe" -d "ReadmePath=$readme" `
        -o $msi (Join-Path $Root 'installer\product.wxs')
    if ($LASTEXITCODE -ne 0) { throw 'wix build failed - copy the error text and send it back for a fix.' }

    Say "Done: $msi"
    exit 0
}
catch {
    Write-Host ''
    Write-Host "INSTALLER BUILD FAILED: $($_.Exception.Message)" -ForegroundColor Red
    exit 1
}
