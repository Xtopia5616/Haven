!macro NSIS_HOOK_PREINSTALL
  ; The SvelteKit bundle uses content-hashed filenames. Remove the previous
  ; app-owned frontend tree before copying the new bundle during an upgrade.
  ; Tauri's built-in running-process check comes after this hook, so run it
  ; first to avoid removing assets while the application is still open.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  RMDir /r "$INSTDIR\ui"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Remove frontend files left by older bundles whose hashed names are not in
  ; this installer's generated Delete list. The built-in check comes after
  ; this hook, so run it first to avoid removing assets while the app is open.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  RMDir /r "$INSTDIR\ui"
!macroend
