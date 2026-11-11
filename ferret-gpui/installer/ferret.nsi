; Ferret's installer (NSIS), built by scripts/release.sh.
;
; It takes the Tauri installer's place: the same folder (%LOCALAPPDATA%\Ferret), the same registry
; entries, the same program name (ferret.exe) and the same arguments from an updater (/P passive,
; /S silent, /UPDATE, /R start Ferret again, /ARGS its arguments). So an installed Tauri copy's
; updater installs this over itself, and Ferret's own updater (src/update.rs) does the same.
; Settings stay where they were (%APPDATA%\com.ferret.app).
;
;   makensis /DVERSION=1.2.3 /DSRC=<folder> /DOUTFILE=<setup.exe> [/DQA] installer\ferret.nsi
;
; SRC holds ferret-gpui.exe (installed as ferret.exe), ferret-indexer.exe and its signature.
; /DQA: a test build that installs as "Ferret QA" (its own registry entries and shortcut, and it
; leaves the default file manager, the search service and the Explorer redirect alone), to try
; the installer next to an installed Ferret.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !error "VERSION is missing: makensis /DVERSION=1.2.3 …"
!endif
!ifndef SRC
  !error "SRC is missing: the folder with ferret-gpui.exe, ferret-indexer.exe and ferret-indexer.exe.sig"
!endif
!ifndef OUTFILE
  !error "OUTFILE is missing: where the setup goes"
!endif

!ifdef QA
  !define PRODUCT "Ferret QA"
!else
  !define PRODUCT "Ferret"
!endif
!define PUBLISHER "meltingice1337"
!define MANUKEY "Software\${PUBLISHER}\${PRODUCT}"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${PRODUCT}"
!define RUNKEY "Software\Microsoft\Windows\CurrentVersion\Run"
; (makensis works from this folder.)
!define ICON "..\..\src-tauri\icons\icon.ico"

Name "${PRODUCT}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\${PRODUCT}"
InstallDirRegKey HKCU "${MANUKEY}" ""
BrandingText "${PRODUCT} ${VERSION}"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${PRODUCT}"
VIAddVersionKey "FileDescription" "${PRODUCT} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "Copyright (C) 2026 ${PUBLISHER}"

!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING

; Passive (/P: a progress window, no questions), an update (/UPDATE), start Ferret after (/R).
Var Passive
Var UpdateMode
Var Restart
Var Args

!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipWhenUpdating
!insertmacro MUI_PAGE_WELCOME
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipWhenUpdating
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_PAGE_CUSTOMFUNCTION_PRE SkipWhenUpdating
!define MUI_FINISHPAGE_RUN "$INSTDIR\ferret.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${PRODUCT}"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Function .onInit
  StrCpy $Passive 0
  StrCpy $UpdateMode 0
  StrCpy $Restart 0
  StrCpy $Args ""
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "/P" $R1
  ${IfNot} ${Errors}
    StrCpy $Passive 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/UPDATE" $R1
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/R" $R1
  ${IfNot} ${Errors}
    StrCpy $Restart 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $R0 "/ARGS" $R1
  ${IfNot} ${Errors}
    StrCpy $Args $R1
  ${EndIf}
  ${If} $Passive = 1
    SetAutoClose true
  ${EndIf}
FunctionEnd

; Welcome, folder and finish pages: not when an updater runs it.
Function SkipWhenUpdating
  ${If} $Passive = 1
  ${OrIf} $UpdateMode = 1
    Abort
  ${EndIf}
FunctionEnd

; Ferret windows (and the Explorer redirect) running from this folder close first, or ferret.exe
; stays locked: the updater closes the window that started it, not the others. Only this
; folder's copies: Ferret run from anywhere else (a development build) is left alone.
!macro CLOSE_FERRET un
Function ${un}CloseFerret
  nsExec::ExecToStack `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "@(Get-Process ferret -ErrorAction SilentlyContinue | Where-Object { $$_.Path -eq '$INSTDIR\ferret.exe' }).Count"`
  Pop $0
  Pop $1
  IntOp $1 $1 + 0
  ${If} $1 > 0
    ${If} $Passive = 0
    ${AndIfNot} ${Silent}
      MessageBox MB_OKCANCEL|MB_ICONINFORMATION "${PRODUCT} is open. It will close to continue." /SD IDOK IDOK +2
      Quit
    ${EndIf}
    DetailPrint "Closing ${PRODUCT}…"
    nsExec::ExecToStack `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "$$p = @(Get-Process ferret -ErrorAction SilentlyContinue | Where-Object { $$_.Path -eq '$INSTDIR\ferret.exe' }); $$p | ForEach-Object { [void]$$_.CloseMainWindow() }; $$p | Wait-Process -Timeout 3 -ErrorAction SilentlyContinue; $$p | Where-Object { -not $$_.HasExited } | Stop-Process -Force -ErrorAction SilentlyContinue; $$p | Wait-Process -Timeout 5 -ErrorAction SilentlyContinue"`
    Pop $0
    Pop $1
  ${EndIf}
FunctionEnd
!macroend
!insertmacro CLOSE_FERRET ""
!insertmacro CLOSE_FERRET "un."

Section "Install"
  SetOutPath "$INSTDIR"
  Call CloseFerret
  SetOverwrite on
  File "/oname=ferret.exe" "${SRC}\ferret-gpui.exe"
  File "/oname=ferret-indexer.exe" "${SRC}\ferret-indexer.exe"
  File "/oname=ferret-indexer.exe.sig" "${SRC}\ferret-indexer.exe.sig"
  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; Where it is (an update finds it here), and its entry in Settings › Apps.
  WriteRegStr HKCU "${MANUKEY}" "" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayName" "${PRODUCT}"
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayIcon" "$\"$INSTDIR\ferret.exe$\""
  WriteRegStr HKCU "${UNINSTKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINSTKEY}" "InstallLocation" "$\"$INSTDIR$\""
  WriteRegStr HKCU "${UNINSTKEY}" "UninstallString" "$\"$INSTDIR\uninstall.exe$\""
  WriteRegStr HKCU "${UNINSTKEY}" "QuietUninstallString" "$\"$INSTDIR\uninstall.exe$\" /S"
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTKEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/M=*.exe /S=0K /G=0" $0 $1 $2
  WriteRegDWORD HKCU "${UNINSTKEY}" "EstimatedSize" "$0"

  ; The Start menu entry: made by a first install; an update keeps whatever the user did with it.
  ${If} $UpdateMode = 0
  ${OrIf} ${FileExists} "$SMPROGRAMS\${PRODUCT}.lnk"
    CreateShortCut "$SMPROGRAMS\${PRODUCT}.lnk" "$INSTDIR\ferret.exe"
  ${EndIf}

!ifndef QA
  ; The Tauri app's web view data: nothing uses it any more.
  RMDir /r "$LOCALAPPDATA\com.ferret.app\EBWebView"
  RMDir /r "$LOCALAPPDATA\com.ferret.app\EBWebView-admin"
  ; While Ferret is the default file manager, its Explorer redirect (closed above) runs again.
  ReadRegStr $0 HKCU "${RUNKEY}" "FerretExplorerRedirect"
  ${If} $0 == "$\"$INSTDIR\ferret.exe$\" --explorer-redirect"
    Exec "$\"$INSTDIR\ferret.exe$\" --explorer-redirect"
  ${EndIf}
!endif
SectionEnd

; After an update: Ferret again, with the arguments it had.
Function .onInstSuccess
  ${If} $Restart = 1
    Exec "$\"$INSTDIR\ferret.exe$\" $Args"
  ${EndIf}
FunctionEnd

Section "Uninstall"
!ifndef QA
  ; Folder windows go back to Explorer (when Ferret was the default file manager) and the
  ; redirect stops, before Ferret goes.
  ${If} ${FileExists} "$INSTDIR\ferret.exe"
    ExecWait "$\"$INSTDIR\ferret.exe$\" --unregister"
  ${EndIf}
  ; The search service and its snapshots (they list every file name on the machine). Removing a
  ; service needs an administrator: asked only when it's installed.
  nsExec::ExecToStack "sc.exe query FerretIndexer"
  Pop $0
  Pop $1
  ${If} $0 == 0
  ${AndIf} ${FileExists} "$INSTDIR\ferret-indexer.exe"
    ExecShellWait "runas" "$INSTDIR\ferret-indexer.exe" "--uninstall" SW_HIDE
  ${EndIf}
!endif
  Call un.CloseFerret
  Delete "$INSTDIR\ferret.exe"
  Delete "$INSTDIR\ferret-indexer.exe"
  Delete "$INSTDIR\ferret-indexer.exe.sig"
  Delete "$INSTDIR\uninstall.exe"
  ; Ferret's caches in its folder (files being edited from a server, remote-edit, stay).
  RMDir /r "$INSTDIR\archive-cache"
  RMDir /r "$INSTDIR\thumbs"
  RMDir /r "$INSTDIR\drag"
  RMDir /r "$INSTDIR\remote-cache"
  Delete "$INSTDIR\index-walk.bin"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${PRODUCT}.lnk"
  DeleteRegKey HKCU "${UNINSTKEY}"
  DeleteRegKey HKCU "${MANUKEY}"
  DeleteRegKey /ifempty HKCU "Software\${PUBLISHER}"
SectionEnd
