!ifndef BUILD_UNINSTALLER
  Var oniAppWasRunning
!endif

# electron-builder sets its default running-app prompts before it inserts this hook.
!macro customAppRunningMessages
  !ifndef BUILD_UNINSTALLER
    StrCpy $oniAppWasRunning 1
  !endif
!macroend

!macro customInstall
  !ifndef DO_NOT_CREATE_START_MENU_SHORTCUT
    ${IfNot} ${FileExists} "$newStartMenuLink"
      !insertmacro createMenuDirectory
      CreateShortCut "$newStartMenuLink" "$appExe" "" "$appExe" 0 "" "" "${APP_DESCRIPTION}"
      ClearErrors
      WinShell::SetLnkAUMI "$newStartMenuLink" "${APP_ID}"
    ${EndIf}
  !endif

  # The template relaunches after a silent install only with --force-run, which scripted /S installs
  # do not pass.
  ${If} ${Silent}
  ${AndIfNot} ${isForceRun}
  ${AndIf} $oniAppWasRunning == 1
    HideWindow
    ${StdUtils.ExecShellAsUser} $0 "$launchLink" "open" ""
  ${EndIf}
!macroend
