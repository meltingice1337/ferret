; Tauri NSIS installer hooks (bundle.windows.nsis.installerHooks).

; Uninstalling Ferret also removes the FerretIndexer service and its snapshots, which
; list every file name on the machine. Updates reinstall over the old version and keep
; the service. Removing a service needs administrator rights, so this asks for UAC, and
; only when the service is actually installed.
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    nsExec::ExecToStack 'sc.exe query FerretIndexer'
    Pop $0 ; exit code: 0 = the service exists
    Pop $1
    ${If} $0 == 0
    ${AndIf} ${FileExists} "$INSTDIR\ferret-indexer.exe"
      ExecShellWait "runas" "$INSTDIR\ferret-indexer.exe" "--uninstall" SW_HIDE
    ${EndIf}
  ${EndIf}
!macroend
