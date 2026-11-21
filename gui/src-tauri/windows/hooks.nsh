; Windows installer hooks (Tauri NSIS). The program that does the work is `periwire self-install` / `self-uninstall`; this only calls it.
; The installer is per-user, so it runs as the person installing: their user PATH gets the install folder, and the uninstaller takes it out again.

!macro NSIS_HOOK_POSTINSTALL
  ; Put `periwire` on the user PATH so PowerShell and cmd find it (a new window is needed). Undo: `periwire self-uninstall`, which the uninstaller runs.
  nsExec::Exec '"$INSTDIR\periwire.exe" self-install --yes --quiet'
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  nsExec::Exec '"$INSTDIR\periwire.exe" self-uninstall --yes'
  Pop $0
!macroend
