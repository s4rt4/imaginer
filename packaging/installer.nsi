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
!define VERSION "0.1.0"
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

Section "Install"
  SetShellVarContext all

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

  ; Tell the shell the association list changed, so the menu is right without a
  ; sign-out.
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
SectionEnd

Section "un.Install"
  SetShellVarContext all
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
  DeleteRegKey HKLM "Software\Classes\${PROGID}"
  DeleteRegKey HKLM "${APPKEY}"
  DeleteRegValue HKLM "Software\RegisteredApplications" "${PRODUCT}"
  System::Call 'shell32::SHChangeNotify(i 0x8000000, i 0, i 0, i 0)'
SectionEnd
