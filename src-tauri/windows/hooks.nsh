; Tauri NSIS installer hooks (bundle.windows.nsis.installerHooks).

; Uninstalling Ferret also removes the FerretIndexer service and its snapshots, which
; list every file name on the machine. Updates reinstall over the old version and keep
; the service. Removing a service needs administrator rights, so this asks for UAC, and
; only when the service is actually installed.
;
; It also gives folder windows back to Explorer when Ferret was the default file manager
; (restoring the registry keys it replaced) and removes the Explorer redirect's sign-in
; entry, so nothing is left pointing at the removed exe. Per-user keys: no UAC.
!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
  ${AndIf} ${FileExists} "$INSTDIR\ferret.exe"
    nsExec::Exec '"$INSTDIR\ferret.exe" --unregister'
    Pop $0
  ${EndIf}
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
