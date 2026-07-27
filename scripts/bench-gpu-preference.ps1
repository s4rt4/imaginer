<#
.SYNOPSIS
    Benchmark startup under each Windows per-app GPU preference.

.DESCRIPTION
    Graphics context creation dominates Imaginer's startup (~873ms of ~1094ms when
    first measured). The leading suspect is hybrid-GPU driver init: on an Optimus
    laptop, binding the discrete adapter means loading NVIDIA's OpenGL ICD, which is
    far heavier than the integrated driver.

    Windows exposes a per-application adapter choice -- Settings > System > Display >
    Graphics -- backed by a registry value. This script sets that value, runs the
    startup benchmark, and repeats for each setting, so the hypothesis is tested
    rather than assumed. The app reports which adapter the GL context actually bound
    to, so the timings can be attributed rather than guessed at.

    The original preference is restored on exit, including the case where no
    preference was set at all.

.EXAMPLE
    .\scripts\bench-gpu-preference.ps1 -Image "C:\photos\test.jpg" -Runs 15
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Image,

    [int]$Runs = 15
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $repoRoot "target\release\imaginer.exe"
$benchScript = Join-Path $PSScriptRoot "bench-startup.ps1"

Write-Host "Building release..." -ForegroundColor Cyan
Push-Location $repoRoot
try {
    cargo build --release --quiet
    if ($LASTEXITCODE -ne 0) { throw "Release build failed" }
}
finally {
    Pop-Location
}
if (-not (Test-Path $exe)) { throw "Binary not found: $exe" }

# Windows keys this by the exact executable path, so it must match what we launch.
$exePath = (Resolve-Path $exe).Path
$prefKey = 'HKCU:\SOFTWARE\Microsoft\DirectX\UserGpuPreferences'

function Get-GpuPreference {
    if (-not (Test-Path $prefKey)) { return $null }
    $entry = Get-ItemProperty -Path $prefKey -Name $exePath -ErrorAction SilentlyContinue
    if ($null -eq $entry) { return $null }
    return $entry.$exePath
}

function Set-GpuPreference {
    param([string]$Value)

    if (-not (Test-Path $prefKey)) {
        New-Item -Path $prefKey -Force | Out-Null
    }

    if ($null -eq $Value) {
        # Absent is a distinct state from any explicit value: it means "let Windows
        # decide", which is the configuration users get by default.
        Remove-ItemProperty -Path $prefKey -Name $exePath -ErrorAction SilentlyContinue
    }
    else {
        Set-ItemProperty -Path $prefKey -Name $exePath -Value $Value -Type String
    }
}

$original = Get-GpuPreference
Write-Host "Original preference: $(if ($null -eq $original) { '(none)' } else { $original })" -ForegroundColor DarkGray

# GpuPreference values are Windows'. 0 is documented as "let Windows decide" and is
# kept distinct from an absent value because the UI writes 0 rather than deleting.
$configurations = @(
    @{ Label = 'no preference set (Windows default)'; Value = $null }
    @{ Label = 'GpuPreference=0 (let Windows decide)'; Value = 'GpuPreference=0;' }
    @{ Label = 'GpuPreference=1 (power saving / iGPU)'; Value = 'GpuPreference=1;' }
    @{ Label = 'GpuPreference=2 (high performance / dGPU)'; Value = 'GpuPreference=2;' }
)

try {
    foreach ($config in $configurations) {
        Write-Host ""
        Write-Host ("=" * 78) -ForegroundColor DarkGray
        Write-Host $config.Label -ForegroundColor Yellow
        Write-Host ("=" * 78) -ForegroundColor DarkGray

        Set-GpuPreference -Value $config.Value

        # -SkipBuild: rebuilding between configurations would change the binary under
        # measurement, and the preference is read at launch, not compiled in.
        & $benchScript -Image $Image -Runs $Runs -SkipBuild
    }
}
finally {
    Set-GpuPreference -Value $original
    Write-Host ""
    Write-Host "Restored preference: $(if ($null -eq $original) { '(none)' } else { $original })" -ForegroundColor DarkGray
}
