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
SectionEnd
