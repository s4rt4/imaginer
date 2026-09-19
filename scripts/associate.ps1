<#
.SYNOPSIS
    Registers Imaginer as an "Open with" candidate and a Default-apps entry for
    every image format it can read, so double-clicking an image can open it.

.DESCRIPTION
    Writes only HKCU, so no administrator rights are needed and nothing outside
    the current user's profile is touched.

    Three things are registered, all of them the documented, supported routes:

      * A ProgId (Software\Classes\Imaginer.AssocFile) whose open command is
        `imaginer.exe "%1"` — the same argv[1] handling a launch from the
        command line already exercises.
      * An OpenWithProgids entry per extension, which is what puts Imaginer in
        Explorer's "Open with" menu and in the "How do you want to open this?"
        dialog.
      * A RegisteredApplications entry with Capabilities\FileAssociations,
        which is what makes Imaginer appear under Settings > Apps > Default
        apps as a whole program, with a "Set default" button covering every
        listed type at once.
      * An Applications\imaginer.exe entry, which is the key Windows writes by
        itself the first time someone picks the exe through "Open with > choose
        another app". Written here so it names the app properly and points at
        the same copy as everything else above: left to Windows it has no
        FriendlyAppName and shows up as a lowercase "imaginer", and if it was
        recorded against a different copy of the exe than the ProgId — an
        installed one against a dev build, say — Explorer lists the two
        separately and the Open-with menu has Imaginer in it twice.

    Setting a default programmatically is deliberately not attempted: since
    Windows 10, HKCU\...\FileExts\<ext>\UserChoice is protected by a hash the
    OS recomputes and rejects, and writing it by hand is the signature of
    adware rather than of an installer. The script registers the choices and
    leaves the one click — "Set default" — to the user, where Windows itself
    records the decision.

    The exe path is baked into the registry. If imaginer.exe moves, re-run
    this script; it rewrites everything from scratch.

.PARAMETER Exe
    Path to imaginer.exe. Defaults to the release build in this repository.

.PARAMETER Uninstall
    Remove the registration instead of adding it.

.EXAMPLE
    .\scripts\associate.ps1

.EXAMPLE
    .\scripts\associate.ps1 -Uninstall
#>
[CmdletBinding()]
param(
    # Left empty here rather than defaulted: $PSScriptRoot is not yet populated
    # while parameters are being bound under `powershell -File`.
    [string] $Exe,
    [switch] $Uninstall
)

$ErrorActionPreference = 'Stop'

if (-not $Exe) {
    $Exe = Join-Path $PSScriptRoot '..\target\release\imaginer.exe'
}

# Mirrors imaginer_core::SUPPORTED_EXTENSIONS — see install-shell-integration.ps1
# for why the two lists must move together. .ff (farbfeld) is left out of the
# association: nothing on this machine produces one, and an association for a
# format nothing opens is clutter in the Open-with list. .psd opens the
# flattened composite, which is what the app decodes. .avif is associated
# because the browsers that serve them will not open one off the disk, so
# double-clicking a downloaded AVIF currently opens nothing at all. .jxl is
# associated for
# the same reason .psd is: files exist in the wild (Unsplash serves them).
$extensions = @('.png', '.jpg', '.jpeg', '.gif', '.bmp', '.webp',
                '.tif', '.tiff', '.ico', '.svg', '.svgz', '.psd', '.jxl', '.avif')

$progId = 'Imaginer.AssocFile'
$classesKey = "HKCU:\Software\Classes"
$appKey = "HKCU:\Software\Imaginer"

if ($Uninstall) {
    foreach ($ext in $extensions) {
        $openWith = "$classesKey\$ext\OpenWithProgids"
        if (Test-Path $openWith) {
            Remove-ItemProperty -Path $openWith -Name $progId -ErrorAction SilentlyContinue
        }
    }
    # Explicit paths, not a loop over a built array: `+` binds tighter than the
    # array comma in PowerShell, and the concatenation this replaced produced
    # one space-joined string that matched no key at all.
    Remove-Item "$classesKey\$progId" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item "$classesKey\Applications\imaginer.exe" -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item $appKey -Recurse -Force -ErrorAction SilentlyContinue
    Remove-ItemProperty -Path 'HKCU:\Software\RegisteredApplications' -Name 'Imaginer' `
        -ErrorAction SilentlyContinue

    Write-Host 'Unregistered Imaginer from the Open-with lists and Default apps.'
    Write-Host 'Any default the user already set through Windows keeps pointing at'
    Write-Host 'the exe where it was registered; re-running this script after moving'
    Write-Host 'imaginer.exe rewrites those paths.'
    return
}

$Exe = (Resolve-Path -LiteralPath $Exe -ErrorAction SilentlyContinue).Path
if (-not $Exe) {
    throw "imaginer.exe not found. Build it first with 'cargo build --release', or pass -Exe <path>."
}

# The ProgId: what "Open with Imaginer" actually runs.
New-Item "$classesKey\$progId" -Force | Out-Null
Set-ItemProperty "$classesKey\$progId" -Name '(default)' -Value 'Imaginer Image'
New-Item "$classesKey\$progId\DefaultIcon" -Force | Out-Null
Set-ItemProperty "$classesKey\$progId\DefaultIcon" -Name '(default)' -Value "$Exe,0"
New-Item "$classesKey\$progId\shell\open\command" -Force | Out-Null
Set-ItemProperty "$classesKey\$progId\shell\open\command" -Name '(default)' -Value "`"$Exe`" `"%1`""

# Per-extension: offer Imaginer in the Open-with menu.
foreach ($ext in $extensions) {
    New-Item "$classesKey\$ext\OpenWithProgids" -Force | Out-Null
    # An empty-string value: OpenWithProgids entries are names, not data.
    New-ItemProperty "$classesKey\$ext\OpenWithProgids" -Name $progId `
        -Value '' -PropertyType String -Force | Out-Null
}

# The exe itself, under the name Windows looks it up by when someone browses to
# it. One entry per file name rather than per path, which is exactly what makes
# it the place to settle which copy of imaginer.exe is *the* one.
$appsKey = "$classesKey\Applications\imaginer.exe"
New-Item "$appsKey\shell\open\command" -Force | Out-Null
Set-ItemProperty "$appsKey\shell\open\command" -Name '(default)' -Value "`"$Exe`" `"%1`""
Set-ItemProperty $appsKey -Name 'FriendlyAppName' -Value 'Imaginer'
# A subkey, one empty value per extension: without it Explorer offers the app for
# every file on the disk, .exe and .dll included, which is how an image viewer
# ends up in a menu it has no business in.
New-Item "$appsKey\SupportedTypes" -Force | Out-Null
foreach ($ext in $extensions) {
    Set-ItemProperty "$appsKey\SupportedTypes" -Name $ext -Value ''
}

# The Default-apps entry: one "Set default" click covering every type.
New-Item "$appKey\Capabilities" -Force | Out-Null
Set-ItemProperty $appKey -Name '(default)' -Value 'Imaginer'
Set-ItemProperty "$appKey\Capabilities" -Name 'ApplicationName' -Value 'Imaginer'
Set-ItemProperty "$appKey\Capabilities" -Name 'ApplicationDescription' `
    -Value 'Fast image viewer and light editor.'
New-Item "$appKey\Capabilities\FileAssociations" -Force | Out-Null
foreach ($ext in $extensions) {
    Set-ItemProperty "$appKey\Capabilities\FileAssociations" -Name $ext -Value $progId
}
New-Item 'HKCU:\Software\RegisteredApplications' -Force | Out-Null
New-ItemProperty 'HKCU:\Software\RegisteredApplications' -Name 'Imaginer' `
    -Value 'Software\Imaginer\Capabilities' -PropertyType String -Force | Out-Null

Write-Host "Registered Imaginer for $($extensions.Count) file types: $($extensions -join ' ')"
Write-Host "  exe   $Exe"
Write-Host ''
Write-Host 'Right-click any image for "Open with > Imaginer". To make it the'
Write-Host 'double-click default: Settings > Apps > Default apps > Imaginer,'
Write-Host 'then "Set default" — Windows records that choice itself; no script'
Write-Host 'can (or should) write it for you.'
