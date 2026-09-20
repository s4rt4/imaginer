<#
.SYNOPSIS
    Adds (or removes) the "Convert with Imaginer" submenu in Explorer's right-click
    menu for every image format Imaginer can read.

.DESCRIPTION
    Writes classic shell verbs under HKCU, so no administrator rights are needed and
    nothing outside the current user's profile is touched. On Windows 11 classic
    verbs live under "Show more options" rather than the short menu; reaching the
    short menu needs a packaged IExplorerCommand COM server, which is deliberately
    not what this does.

    The submenu is built with the empty-SubCommands trick: a parent verb whose
    `SubCommands` value is an empty string makes Explorer enumerate the verbs in the
    parent's own nested `shell` key. The documented alternative routes through
    HKLM\...\Explorer\CommandStore, which would need administrator rights for no
    gain here.

    Since 0.2.4 the installer writes the same submenu under HKLM, pointing at the
    installed exe, so an ordinary install already has it on every account. This
    script stays for pointing the menu at a development build instead: HKCU wins
    over HKLM in the merged view, so what it writes shadows the installed entries
    until -Uninstall takes it back out.

    Each entry passes a single file, because that is all a classic verb gets:
    selecting twelve images launches twelve processes. `--collect` is what puts them
    back together — the first process to start collects the others' paths over a
    named pipe and converts the whole selection, asking once where to put it.

.PARAMETER Exe
    Path to imaginer.exe. Defaults to the release build in this repository.

.PARAMETER Uninstall
    Remove the entries instead of adding them.

.EXAMPLE
    .\scripts\install-shell-integration.ps1

.EXAMPLE
    .\scripts\install-shell-integration.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    # Left empty here rather than defaulted: $PSScriptRoot is not yet populated while
    # parameters are being bound under `powershell -File`, so the default is worked
    # out below instead.
    [string] $Exe,
    [switch] $Uninstall
)

$ErrorActionPreference = 'Stop'

if (-not $Exe) {
    $Exe = Join-Path $PSScriptRoot '..\target\release\imaginer.exe'
}

# Mirrors imaginer_core::SUPPORTED_EXTENSIONS. A format added there and not here
# simply gets no menu entry, which is a missing feature rather than a broken one.
# .ico is on the list as a source: converting an existing icon to PNG or WebP is a
# thing people want, and the app writes .ico either way.
# .svg is the one worth having here above all: right-click a logo and get an .ico
# or a WebP, which is the errand that sends people to an online converter.
# .psd converts the flattened composite, which is what the app decodes.
# .avif decodes via rav1d, and converting out of it is the useful direction:
# nothing else on this machine opens one, and the web serves them.
# .jxl decodes via jxl-oxide; conversion out of it is the useful direction,
# since nothing this machine produces writes JPEG XL.
$extensions = @('.png', '.jpg', '.jpeg', '.gif', '.bmp', '.webp',
                '.tif', '.tiff', '.ico', '.ff', '.svg', '.svgz', '.psd', '.jxl',
                '.avif')

# What the submenu offers. The quality is fixed because a context menu has no room
# for a slider: 90 is the CLI default and the point past which WebP and JPEG grow
# much faster than anything visible improves. For a specific quality, use the export
# panel in the app or `imaginer --convert <format> --quality N`.
$targets = @(
    [pscustomobject]@{ Order = '01'; Format = 'webp'; Label = 'WebP' }
    [pscustomobject]@{ Order = '02'; Format = 'ico';  Label = 'Icon (.ico)' }
    [pscustomobject]@{ Order = '03'; Format = 'png';  Label = 'PNG' }
    [pscustomobject]@{ Order = '04'; Format = 'jpg';  Label = 'JPEG' }
)

# The parent verb's key name. Also what gets deleted on uninstall, so it must not
# change without a thought for anyone who installed the previous name.
$verb = 'Imaginer.Convert'

function Get-VerbKey {
    param([string] $Extension)
    "HKCU:\Software\Classes\SystemFileAssociations\$Extension\shell\$verb"
}

if ($Uninstall) {
    $removed = 0
    foreach ($ext in $extensions) {
        $key = Get-VerbKey $ext
        if (Test-Path $key) {
            Remove-Item $key -Recurse -Force
            $removed++
        }
    }
    Write-Host "Removed the Imaginer submenu from $removed of $($extensions.Count) file types."
    Write-Host 'Explorer picks this up immediately; no restart or sign-out needed.'
    return
}

$Exe = (Resolve-Path -LiteralPath $Exe -ErrorAction SilentlyContinue).Path
if (-not $Exe) {
    throw "imaginer.exe not found. Build it first with 'cargo build --release', or pass -Exe <path>."
}

foreach ($ext in $extensions) {
    $key = Get-VerbKey $ext

    # Rewritten from scratch rather than merged into, so an entry removed from
    # $targets actually disappears instead of lingering from an earlier install.
    if (Test-Path $key) { Remove-Item $key -Recurse -Force }
    New-Item $key -Force | Out-Null

    New-ItemProperty $key -Name 'MUIVerb' -Value 'Convert with Imaginer' -PropertyType String -Force | Out-Null
    # Empty, not absent: this is the signal to look for sub-verbs in the nested
    # `shell` key below. Removing it collapses the submenu into a dead entry.
    New-ItemProperty $key -Name 'SubCommands' -Value '' -PropertyType String -Force | Out-Null
    New-ItemProperty $key -Name 'Icon' -Value "$Exe,0" -PropertyType String -Force | Out-Null

    foreach ($target in $targets) {
        # Explorer orders sub-verbs by key name, hence the numeric prefix.
        $sub = Join-Path $key "shell\$($target.Order)_$($target.Format)"
        New-Item $sub -Force | Out-Null
        New-ItemProperty $sub -Name 'MUIVerb' -Value $target.Label -PropertyType String -Force | Out-Null

        $command = Join-Path $sub 'command'
        New-Item $command -Force | Out-Null
        Set-ItemProperty $command -Name '(default)' `
            -Value "`"$Exe`" --convert $($target.Format) --collect `"%1`""
    }
}

Write-Host "Installed for $($extensions.Count) file types: $($extensions -join ' ')"
Write-Host "  exe      $Exe"
Write-Host "  formats  $(($targets | ForEach-Object { $_.Label }) -join ', ')"
Write-Host ''
Write-Host 'Right-click an image, choose "Show more options" on Windows 11, then'
Write-Host '"Convert with Imaginer". Select several files and they convert as one batch.'
Write-Host ''
Write-Host 'Note: Explorer refuses to invoke a classic verb on more than 15 selected'
Write-Host 'files at once (MultipleInvokePromptMinimum). Beyond that, convert in the app.'
