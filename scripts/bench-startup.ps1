<#
.SYNOPSIS
    Measure Imaginer's startup time: launch to the image being on screen.

.DESCRIPTION
    Startup is the one metric that justifies this project, so it is measured from
    the first milestone rather than profiled at the end -- a commit that costs 40ms
    should be caught when it lands.

    Runs the release binary N times against an image, reading the milestones the
    app prints under IMAGINER_TRACE_STARTUP, and reports the median. Median rather
    than mean because the occasional scheduler-induced outlier says nothing useful.

    NOTE: this measures WARM start -- the binary, its DLLs and the image are all in
    the OS page cache after the first run. Cold start (first launch after a reboot)
    is substantially slower and cannot be measured this way. Keep both numbers in
    mind before claiming a figure.

.EXAMPLE
    .\scripts\bench-startup.ps1 -Image "C:\photos\test.jpg" -Runs 20
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Image,

    [int]$Runs = 20,

    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot

if (-not (Test-Path $Image)) {
    throw "Image not found: $Image"
}

if (-not $SkipBuild) {
    Write-Host "Building release..." -ForegroundColor Cyan
    cargo build --release --quiet
    if ($LASTEXITCODE -ne 0) { throw "Release build failed" }
}

$exe = Join-Path $repoRoot "target\release\imaginer.exe"
if (-not (Test-Path $exe)) { throw "Binary not found: $exe" }

$env:IMAGINER_TRACE_STARTUP = "1"
$env:IMAGINER_EXIT_AFTER_FIRST_FRAME = "1"

# Milestone name -> samples. Ordered so the report reads chronologically.
$milestones = [ordered]@{
    'context_ready' = [System.Collections.Generic.List[double]]::new()
    'theme_ready'   = [System.Collections.Generic.List[double]]::new()
    'first_image'   = [System.Collections.Generic.List[double]]::new()
    'first_frame'   = [System.Collections.Generic.List[double]]::new()
}

# Milestones go to stderr, which cannot be captured with `2>&1` here: in Windows
# PowerShell that wraps every line in an ErrorRecord and reports failure even when
# the process exits 0. Redirecting to a file sidesteps it.
$errFile = [System.IO.Path]::GetTempFileName()

function Invoke-Run {
    param([switch]$Record)

    Start-Process -FilePath $exe -ArgumentList $Image `
        -RedirectStandardError $errFile -NoNewWindow -Wait | Out-Null

    if (-not $Record) { return }

    foreach ($line in (Get-Content $errFile)) {
        if ($line -match 'startup:\s+(\w+)\s+([\d.]+)ms') {
            $name = $matches[1]
            if ($milestones.Contains($name)) {
                $milestones[$name].Add([double]$matches[2])
            }
        }
    }
}

# One unmeasured run so the page cache is warm for all the runs that count --
# otherwise the first sample is an outlier that skews nothing but confuses.
Write-Host "Warming up..." -ForegroundColor DarkGray
Invoke-Run

Write-Host "Running $Runs iterations..." -ForegroundColor Cyan
for ($i = 1; $i -le $Runs; $i++) {
    Invoke-Run -Record
    Write-Host "." -NoNewline
}
Write-Host ""
Remove-Item $errFile -ErrorAction SilentlyContinue

function Get-Stats {
    param([System.Collections.Generic.List[double]]$Values, [string]$Label)

    if ($Values.Count -eq 0) {
        Write-Host ("{0,-14} no samples" -f $Label) -ForegroundColor Yellow
        return
    }

    $sorted = $Values | Sort-Object
    $median = if ($sorted.Count % 2 -eq 1) {
        $sorted[[int](($sorted.Count - 1) / 2)]
    }
    else {
        ($sorted[$sorted.Count / 2 - 1] + $sorted[$sorted.Count / 2]) / 2
    }

    Write-Host ("{0,-14} median {1,7:F1}ms   min {2,7:F1}ms   max {3,7:F1}ms   n={4}" -f `
            $Label, $median, $sorted[0], $sorted[-1], $sorted.Count)
}

Write-Host ""
Write-Host "Warm start, $(Split-Path -Leaf $Image)" -ForegroundColor Green
foreach ($name in $milestones.Keys) {
    Get-Stats -Values $milestones[$name] -Label $name
}
Write-Host ""
Write-Host "first_image is the number that matters -- first_frame can be an empty canvas." -ForegroundColor DarkGray
