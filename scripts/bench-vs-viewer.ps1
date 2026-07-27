<#
.SYNOPSIS
    Compare Imaginer's launch time against another image viewer, measured identically.

.DESCRIPTION
    Imaginer's internal milestones cannot be compared against a viewer we did not
    build, so this measures both from the outside with the same crude metric: wall
    time from process start until the process owns a main window.

    That metric is APPROXIMATE and it flatters nobody consistently -- a window can
    exist before it has painted anything, and different toolkits create and show
    windows at different points in their startup. Treat a small difference as noise;
    it is only meaningful when one viewer is several hundred ms apart from the other.
    Imaginer's own IMAGINER_TRACE_STARTUP milestones remain the accurate numbers for
    tracking Imaginer against itself.

.EXAMPLE
    .\scripts\bench-vs-viewer.ps1 -Image "C:\photos\test.jpg" -Other "C:\Program Files\nomacs\bin\nomacs.exe"
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Image,

    [Parameter(Mandatory = $true)]
    [string]$Other,

    [int]$Runs = 10
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$imaginer = Join-Path $repoRoot "target\release\imaginer.exe"

foreach ($required in @($Image, $Other, $imaginer)) {
    if (-not (Test-Path $required)) { throw "Not found: $required" }
}

# The trace/exit flags would change what is being measured, and the other viewer has
# no equivalent, so both run in their normal configuration.
foreach ($name in 'IMAGINER_TRACE_STARTUP', 'IMAGINER_EXIT_AFTER_FIRST_FRAME', 'IMAGINER_TRACE_INIT') {
    if (Test-Path "Env:$name") { Remove-Item "Env:$name" }
}

function Measure-Launch {
    param([string]$Exe, [string]$Argument)

    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $process = Start-Process -FilePath $Exe -ArgumentList $Argument -PassThru

    $elapsed = $null
    while ($stopwatch.ElapsedMilliseconds -lt 20000) {
        $process.Refresh()
        if ($process.HasExited) { break }
        if ($process.MainWindowHandle -ne 0) {
            $elapsed = $stopwatch.Elapsed.TotalMilliseconds
            break
        }
    }
    $stopwatch.Stop()

    # Close politely first: a killed viewer can leave state that slows its next start.
    if (-not $process.HasExited) {
        $process.CloseMainWindow() | Out-Null
        if (-not $process.WaitForExit(3000)) { $process.Kill() }
    }
    # Windows will not reuse a just-freed window position/state instantly; a short
    # settle keeps consecutive runs independent.
    Start-Sleep -Milliseconds 400

    return $elapsed
}

function Get-Median {
    param([System.Collections.Generic.List[double]]$Values)

    $sorted = $Values | Sort-Object
    if ($sorted.Count % 2 -eq 1) { return $sorted[[int](($sorted.Count - 1) / 2)] }
    return ($sorted[$sorted.Count / 2 - 1] + $sorted[$sorted.Count / 2]) / 2
}

$targets = [ordered]@{
    'Imaginer'              = $imaginer
    (Split-Path -Leaf $Other) = $Other
}

$results = [ordered]@{}
foreach ($name in $targets.Keys) {
    Write-Host "Measuring $name..." -ForegroundColor Cyan -NoNewline
    $samples = [System.Collections.Generic.List[double]]::new()

    Measure-Launch -Exe $targets[$name] -Argument $Image | Out-Null  # warm-up

    for ($i = 1; $i -le $Runs; $i++) {
        $sample = Measure-Launch -Exe $targets[$name] -Argument $Image
        if ($null -ne $sample) { $samples.Add($sample) }
        Write-Host "." -NoNewline
    }
    Write-Host ""
    $results[$name] = $samples
}

Write-Host ""
Write-Host "Warm start to window, $(Split-Path -Leaf $Image)" -ForegroundColor Green
foreach ($name in $results.Keys) {
    $samples = $results[$name]
    if ($samples.Count -eq 0) {
        Write-Host ("{0,-16} no samples" -f $name) -ForegroundColor Yellow
        continue
    }
    $sorted = $samples | Sort-Object
    Write-Host ("{0,-16} median {1,7:F1}ms   min {2,7:F1}ms   max {3,7:F1}ms   n={4}" -f `
            $name, (Get-Median -Values $samples), $sorted[0], $sorted[-1], $sorted.Count)
}
Write-Host ""
Write-Host "Approximate: 'has a window' is not 'has painted the image'. Differences under" -ForegroundColor DarkGray
Write-Host "~100ms mean nothing here." -ForegroundColor DarkGray
