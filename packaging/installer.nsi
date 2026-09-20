; Imaginer installer — NSIS 3 + MUI2.
;
; Modeled on the Tauri NSIS bundler output mark-hulk ships: the app's own icon
; on the installer and uninstaller, the logo across the header and welcome
; panel, a Program Files install with start-menu and desktop shortcuts, an
; Add/Remove Programs entry, and a complete uninstaller. The images and the
; icon are generated from the SVG sources by
; `cargo run --release -p imaginer-ui --example make-installer-assets`.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!include "MUI2.nsh"

!define PRODUCT "Imaginer"
!define VERSION "0.2.4"
!define EXE "imaginer.exe"
!define UNKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}"
!define PROGID "Imaginer.AssocFile"
!define CAPS "Software\${PRODUCT}\Capabilities"
!define APPKEY "Software\Classes\Applications\${EXE}"

; The formats offered in Open-with, mirroring `scripts\associate.ps1` — which
; mirrors `imaginer_core::SUPPORTED_EXTENSIONS` in turn. Three lists that have to
; move together; farbfeld is deliberately in none of them.
!macro EachExtension macro
  !insertmacro ${macro} ".png"
  !insertmacro ${macro} ".jpg"
  !insertmacro ${macro} ".jpeg"
  !insertmacro ${macro} ".gif"
  !insertmacro ${macro} ".bmp"
  !insertmacro ${macro} ".webp"
  !insertmacro ${macro} ".tif"
  !insertmacro ${macro} ".tiff"
  !insertmacro ${macro} ".ico"
  !insertmacro ${macro} ".svg"
  !insertmacro ${macro} ".svgz"
  !insertmacro ${macro} ".psd"
  !insertmacro ${macro} ".jxl"
  !insertmacro ${macro} ".avif"
!macroend

!macro RegisterExtension ext
  ; A name in a list, not a default: an OpenWithProgids entry offers the app
  ; without taking the file type away from whatever already owns it.
  WriteRegStr HKLM "Software\Classes\${ext}\OpenWithProgids" "${PROGID}" ""
  WriteRegStr HKLM "${CAPS}\FileAssociations" "${ext}" "${PROGID}"
  WriteRegStr HKLM "${APPKEY}\SupportedTypes" "${ext}" ""
!macroend

!macro UnregisterExtension ext
  DeleteRegValue HKLM "Software\Classes\${ext}\OpenWithProgids" "${PROGID}"
!macroend

; The "Convert with Imaginer" submenu, which until 0.2.4 only ever existed on the
; machine that built the app: `scripts\install-shell-integration.ps1` writes it
; under HKCU and points it at `target\release\imaginer.exe`, so it was a
; developer convenience that died with a `cargo clean` and travelled nowhere. The
; same verbs are written here instead, under HKLM and pointing at $INSTDIR, which
; puts them on every account of every machine that runs the installer.
;
; HKCU\Software\Classes wins over HKLM\Software\Classes in the merged view, so a
; leftover entry from that script shadows this one and keeps pointing at a build
; folder. Run the script with -Uninstall once and the installed menu takes over.
;
; Structure, format list and quality are the script's: a parent verb whose empty
; `SubCommands` value makes Explorer enumerate the nested `shell` key, one entry
; per output format, ordered by key name, converting at the CLI's default quality
; because a context menu has no room for a slider. `--collect` is what turns a
; multi-file selection — one process per file, as classic verbs are invoked —
; back into a single batch that asks once where to put the results.
!define CONVERT "Imaginer.Convert"
!define FILETYPES "Software\Classes\SystemFileAssociations"

!macro ConvertTarget ext order format label
  WriteRegStr HKLM "${FILETYPES}\${ext}\shell\${CONVERT}\shell\${order}_${format}" "MUIVerb" "${label}"
  WriteRegStr HKLM "${FILETYPES}\${ext}\shell\${CONVERT}\shell\${order}_${format}\command" "" '"$INSTDIR\${EXE}" --convert ${format} --collect "%1"'
!macroend

!macro RegisterConvert ext
  ; Written fresh rather than merged into: an older install's format list comes
  ; out with the key, instead of lingering as an entry nothing writes any more.
  DeleteRegKey HKLM "${FILETYPES}\${ext}\shell\${CONVERT}"
  WriteRegStr HKLM "${FILETYPES}\${ext}\shell\${CONVERT}" "MUIVerb" "Convert with ${PRODUCT}"
  ; Empty, not absent: this is the signal to look for sub-verbs in the nested
  ; `shell` key. Remove it and the submenu collapses into a dead entry.
  WriteRegStr HKLM "${FILETYPES}\${ext}\shell\${CONVERT}" "SubCommands" ""
  WriteRegStr HKLM "${FILETYPES}\${ext}\shell\${CONVERT}" "Icon" "$INSTDIR\${EXE},0"
  !insertmacro ConvertTarget "${ext}" "01" "webp" "WebP"
  !insertmacro ConvertTarget "${ext}" "02" "ico" "Icon (.ico)"
  !insertmacro ConvertTarget "${ext}" "03" "png" "PNG"
  !insertmacro ConvertTarget "${ext}" "04" "jpg" "JPEG"
!macroend

!macro UnregisterConvert ext
  DeleteRegKey HKLM "${FILETYPES}\${ext}\shell\${CONVERT}"
!macroend

!define MUI_ICON "imaginer.ico"
!define MUI_UNICON "imaginer.ico"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "header.bmp"
!define MUI_HEADERIMAGE_RIGHT
!define MUI_WELCOMEFINISHPAGE_BITMAP "sidebar.bmp"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Run ${PRODUCT}"

Name "${PRODUCT} ${VERSION}"
OutFile "..\dist\Imaginer-${VERSION}-setup.exe"
InstallDir "$PROGRAMFILES64\${PRODUCT}"
InstallDirRegKey HKLM "Software\${PRODUCT}" "InstallDir"
RequestExecutionLevel admin

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; Read the previous install location from the view the writes below use.
;
; `InstallDirRegKey` cannot be told a registry view, and NSIS is a 32-bit
; process, so it reads the 32-bit one — which is where versions up to 0.2.0 put
; everything by accident. Both are consulted: the 64-bit value if this has
; installed since the fix, the 32-bit one if it has not.
Function .onInit
  SetRegView 64
  ReadRegStr $0 HKLM "Software\${PRODUCT}" "InstallDir"
  StrCmp $0 "" +2 0
  StrCpy $INSTDIR $0
FunctionEnd

Section "Install"
  SetShellVarContext all

  ; Everything below goes in the 64-bit view, because this is a 64-bit program
  ; installed under $PROGRAMFILES64. Without this NSIS, being 32-bit, is
  ; redirected into WOW6432Node — and versions up to 0.2.0 were: the Default
  ; apps entry pointed `RegisteredApplications\Imaginer` at a Capabilities key
  ; that, read by 64-bit Windows, was not there. The file associations survived
  ; that only because HKLM\SOFTWARE\Classes is shared between the two views
  ; rather than redirected, which is what kept the fault invisible.
  SetRegView 64

  ; The 32-bit copies an earlier installer left. Removed before anything is
  ; written, so an upgrade does not end up listed twice in Apps & Features.
  SetRegView 32
  DeleteRegKey HKLM "${UNKEY}"
  DeleteRegKey HKLM "Software\${PRODUCT}"
  DeleteRegValue HKLM "Software\RegisteredApplications" "${PRODUCT}"
  SetRegView 64

  ; An image still open in the previous install would hold the exe and fail
  ; the copy; killing it is safe whether or not it is running.
  nsExec::Exec 'taskkill /IM ${EXE} /F'
  Pop $0
  Sleep 300

  SetOutPath "$INSTDIR"
  File "/oname=${EXE}" "..\target\release\imaginer.exe"
  File "/oname=LICENSE" "..\LICENSE"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  CreateDirectory "$SMPROGRAMS\${PRODUCT}"
  CreateShortcut "$SMPROGRAMS\${PRODUCT}\${PRODUCT}.lnk" "$INSTDIR\${EXE}"
  CreateShortcut "$SMPROGRAMS\${PRODUCT}\Uninstall ${PRODUCT}.lnk" "$INSTDIR\uninstall.exe"
  CreateShortcut "$DESKTOP\${PRODUCT}.lnk" "$INSTDIR\${EXE}"

  WriteRegStr HKLM "Software\${PRODUCT}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNKEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKLM "${UNKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNKEY}" "Publisher" "Sarta"
  WriteRegStr HKLM "${UNKEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKLM "${UNKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNKEY}" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "${UNKEY}" "QuietUninstallString" "$INSTDIR\uninstall.exe /S"
  WriteRegDWORD HKLM "${UNKEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNKEY}" "NoRepair" 1

  ; File associations. Without these an installed Imaginer is not in the
  ; Open-with menu at all, and the way anyone reaches for it instead — "choose
  ; another app", browse to the exe — has Windows write a nameless
  ; Applications\${EXE} key of its own. That key is keyed by file name rather
  ; than by path, so once a second copy of the exe has been registered somewhere
  ; else the menu ends up with two Imaginers in it, one of them a lowercase
  ; "imaginer" opening a different build. Writing all three routes here, at one
  ; path, is what keeps it to one entry.
  WriteRegStr HKLM "Software\Classes\${PROGID}" "" "Imaginer Image"
  WriteRegStr HKLM "Software\Classes\${PROGID}\DefaultIcon" "" "$INSTDIR\${EXE},0"
  WriteRegStr HKLM "Software\Classes\${PROGID}\shell\open\command" "" '"$INSTDIR\${EXE}" "%1"'

  WriteRegStr HKLM "${APPKEY}" "FriendlyAppName" "${PRODUCT}"
  WriteRegStr HKLM "${APPKEY}\shell\open\command" "" '"$INSTDIR\${EXE}" "%1"'

  WriteRegStr HKLM "${CAPS}" "ApplicationName" "${PRODUCT}"
  WriteRegStr HKLM "${CAPS}" "ApplicationDescription" "Fast image viewer and light editor."
  WriteRegStr HKLM "Software\RegisteredApplications" "${PRODUCT}" "${CAPS}"

  !insertmacro EachExtension RegisterExtension
  !insertmacro EachExtension RegisterConvert

  ; Tell the shell the association list changed, so the menu is right without a
  ; sign-out.
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
SectionEnd

Section "un.Install"
  SetShellVarContext all
  SetRegView 64
  nsExec::Exec 'taskkill /IM ${EXE} /F'
  Pop $0
  Sleep 300

  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${PRODUCT}\*.lnk"
  RMDir "$SMPROGRAMS\${PRODUCT}"
  Delete "$DESKTOP\${PRODUCT}.lnk"
  DeleteRegKey HKLM "${UNKEY}"
  DeleteRegKey HKLM "Software\${PRODUCT}"

  ; Symmetrical with the install: every key written above comes out, including
  ; the per-extension names, which would otherwise leave a dead Imaginer in the
  ; Open-with menu of every image on the machine.
  !insertmacro EachExtension UnregisterExtension
  !insertmacro EachExtension UnregisterConvert
  DeleteRegKey HKLM "Software\Classes\${PROGID}"
  DeleteRegKey HKLM "${APPKEY}"
  DeleteRegValue HKLM "Software\RegisteredApplications" "${PRODUCT}"

  ; And whatever a pre-fix installer left in the other view, so uninstalling
  ; really does leave nothing behind.
  SetRegView 32
  DeleteRegKey HKLM "${UNKEY}"
  DeleteRegKey HKLM "Software\${PRODUCT}"
  DeleteRegValue HKLM "Software\RegisteredApplications" "${PRODUCT}"
  SetRegView 64

  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
SectionEnd
