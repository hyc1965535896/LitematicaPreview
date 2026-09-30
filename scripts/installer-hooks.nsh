!include "LogicLib.nsh"
!include "nsDialogs.nsh"
!include "${__FILEDIR__}\..\App\src-tauri\gen\installer-extensions.nsh"

; installer.nsi inserts this file before defining PRODUCTNAME and other Tauri values.
; Callbacks therefore use only these local variables and NSIS APIs.
Var LPOptionsInitialized
Var LPAssociationDialog
Var LPRegisterControl
Var LPRegisterState
Var LPSettingsControl
Var LPSettingsState
Var LPPassiveMode
Var LPSelectedExtensions
Var LPEventControl
Var LPAssociationWarning

!macro LP_DECLARE_EXTENSION ID EXTENSION
  Var LP_${ID}_Control
  Var LP_${ID}_State
!macroend
!insertmacro LP_FOREACH_EXTENSION LP_DECLARE_EXTENSION

!macro LP_INITIALIZE_EXTENSION ID EXTENSION
  StrCpy $LP_${ID}_State ${BST_CHECKED}
!macroend

Function LPInitializeOptions
  ${If} $LPOptionsInitialized != 1
    StrCpy $LPOptionsInitialized 1
    StrCpy $LPRegisterState ${BST_CHECKED}
    StrCpy $LPSettingsState ${BST_UNCHECKED}
    StrCpy $LPSelectedExtensions ""
    StrCpy $LPPassiveMode 0
    !insertmacro LP_FOREACH_EXTENSION LP_INITIALIZE_EXTENSION
    Push $0
    ClearErrors
    ${GetOptions} $CMDLINE "/P" $0
    ${IfNot} ${Errors}
      StrCpy $LPPassiveMode 1
    ${EndIf}
    ClearErrors
    Pop $0
  ${EndIf}
FunctionEnd

!macro LP_CREATE_EXTENSION ID EXTENSION
  !define /math LP_EXTENSION_ROW ${LP_EXTENSION_INDEX} / 2
  !define /math LP_EXTENSION_Y ${LP_EXTENSION_ROW} * 16
  !define /redef /math LP_EXTENSION_Y ${LP_EXTENSION_Y} + 53
  !define /math LP_EXTENSION_COLUMN ${LP_EXTENSION_INDEX} % 2
  !if ${LP_EXTENSION_COLUMN} == 0
    ${NSD_CreateCheckbox} 10u ${LP_EXTENSION_Y}u 130u 12u "${EXTENSION}"
  !else
    ${NSD_CreateCheckbox} 155u ${LP_EXTENSION_Y}u 130u 12u "${EXTENSION}"
  !endif
  Pop $LP_${ID}_Control
  ${NSD_SetState} $LP_${ID}_Control $LP_${ID}_State
  ${NSD_OnClick} $LP_${ID}_Control LPOptionsChanged
  !define /redef /math LP_EXTENSION_INDEX ${LP_EXTENSION_INDEX} + 1
  !undef LP_EXTENSION_ROW
  !undef LP_EXTENSION_Y
  !undef LP_EXTENSION_COLUMN
!macroend

!macro LP_ENABLE_EXTENSION ID EXTENSION
  EnableWindow $LP_${ID}_Control $LPRegisterState
!macroend

!macro LP_CAPTURE_EXTENSION ID EXTENSION
  ${NSD_GetState} $LP_${ID}_Control $LP_${ID}_State
  ${If} $LP_${ID}_State == ${BST_CHECKED}
    ${If} $LPSelectedExtensions == ""
      StrCpy $LPSelectedExtensions "${EXTENSION}"
    ${Else}
      StrCpy $LPSelectedExtensions "$LPSelectedExtensions,${EXTENSION}"
    ${EndIf}
  ${EndIf}
!macroend

Function LPCaptureOptions
  ${NSD_GetState} $LPRegisterControl $LPRegisterState
  ${NSD_GetState} $LPSettingsControl $LPSettingsState
  StrCpy $LPSelectedExtensions ""
  !insertmacro LP_FOREACH_EXTENSION LP_CAPTURE_EXTENSION
FunctionEnd

Function LPUpdateEnabledControls
  !insertmacro LP_FOREACH_EXTENSION LP_ENABLE_EXTENSION
  EnableWindow $LPSettingsControl $LPRegisterState
FunctionEnd

Function LPOptionsChanged
  Pop $LPEventControl
  ; Capture every click so Back restores the user's choices.
  Call LPCaptureOptions
  Call LPUpdateEnabledControls
FunctionEnd


Function LPAssociationPage
  Call LPInitializeOptions
  ${If} ${Silent}
  ${OrIf} $LPPassiveMode == 1
    Abort
  ${EndIf}

  !insertmacro MUI_HEADER_TEXT "选择文件关联" "Litematica Preview 的可选设置"
  nsDialogs::Create 1018
  Pop $LPAssociationDialog
  ${If} $LPAssociationDialog == error
    MessageBox MB_OK|MB_ICONSTOP "安装程序无法显示文件关联选项，请重新运行安装程序。"
    Quit
  ${EndIf}

  ${NSD_CreateLabel} 0 0 100% 28u "Windows 会保留你当前的默认应用。安装程序会把 Litematica Preview 添加到所选格式的“打开方式”中，之后可在 Windows 设置中选择默认应用。"
  Pop $LPEventControl
  ${NSD_CreateCheckbox} 0 34u 100% 12u "为这些文件类型注册 Litematica Preview"
  Pop $LPRegisterControl
  ${NSD_SetState} $LPRegisterControl $LPRegisterState
  ${NSD_OnClick} $LPRegisterControl LPOptionsChanged

  !define LP_EXTENSION_INDEX 0
  !insertmacro LP_FOREACH_EXTENSION LP_CREATE_EXTENSION
  !undef LP_EXTENSION_INDEX

  ${NSD_CreateCheckbox} 0 124u 100% 16u "安装后打开“默认应用”设置"
  Pop $LPSettingsControl
  ${NSD_SetState} $LPSettingsControl $LPSettingsState
  ${NSD_OnClick} $LPSettingsControl LPOptionsChanged
  Call LPUpdateEnabledControls
  nsDialogs::Show
FunctionEnd

Function LPAssociationPageLeave
  Call LPCaptureOptions
  ${If} $LPRegisterState == ${BST_CHECKED}
  ${AndIf} $LPSelectedExtensions == ""
    MessageBox MB_OK|MB_ICONEXCLAMATION "请至少选择一种文件类型，或取消勾选注册后继续。"
    Abort
  ${EndIf}
FunctionEnd

Function LPShowAssociationWarning
  DetailPrint "$LPAssociationWarning"
  ${IfNot} ${Silent}
  ${AndIf} $LPPassiveMode != 1
    MessageBox MB_OK|MB_ICONEXCLAMATION "$LPAssociationWarning" /SD IDOK
  ${EndIf}
FunctionEnd

Function LPApplyPostInstall
  Push $0
  ClearErrors
  ${If} $LPRegisterState == ${BST_CHECKED}
    DetailPrint "正在注册所选文件类型：$LPSelectedExtensions"
    ExecWait '"$INSTDIR\LitematicaPreview.exe" --register-extensions "$LPSelectedExtensions"' $0
  ${Else}
    DetailPrint "未启用文件注册。仅移除本次安装所拥有的关联。"
    ExecWait '"$INSTDIR\LitematicaPreview.exe" --unregister' $0
  ${EndIf}
  ${If} ${Errors}
    StrCpy $LPAssociationWarning "Litematica Preview 已安装，但安装程序无法启动文件关联更新。你可以稍后在应用内更改文件关联。"
    Call LPShowAssociationWarning
  ${ElseIf} $0 != 0
    DetailPrint "文件关联更新返回了 $0。"
    StrCpy $LPAssociationWarning "Litematica Preview 已安装，但文件关联未能完全更新。可能是另一份已安装副本占用了关联。你可以稍后在应用内更改文件关联。"
    Call LPShowAssociationWarning
  ${Else}
    DetailPrint "文件关联选项已应用。Windows 的默认应用选择保持不变。"
  ${EndIf}

  ${IfNot} ${Silent}
  ${AndIf} $LPPassiveMode != 1
  ${AndIf} $LPRegisterState == ${BST_CHECKED}
  ${AndIf} $LPSettingsState == ${BST_CHECKED}
    ClearErrors
    DetailPrint "正在按要求打开 Windows“默认应用”设置。"
    ExecWait '"$INSTDIR\LitematicaPreview.exe" --default-apps' $0
    ${If} ${Errors}
      StrCpy $LPAssociationWarning "安装程序无法打开 Windows 设置。请打开“设置 > 应用 > 默认应用”来选择 Litematica Preview。"
      Call LPShowAssociationWarning
    ${ElseIf} $0 != 0
      StrCpy $LPAssociationWarning "无法打开 Windows 设置。请打开“设置 > 应用 > 默认应用”来选择 Litematica Preview。"
      Call LPShowAssociationWarning
    ${EndIf}
  ${EndIf}
  ClearErrors
  Pop $0
FunctionEnd

!macro NSIS_HOOK_PREINSTALL
  Call LPInitializeOptions
  ${If} ${Silent}
  ${OrIf} $LPPassiveMode == 1
    StrCpy $LPRegisterState ${BST_UNCHECKED}
  ${EndIf}
!macroend

; The app owns associations and Windows UserChoice handling.
; The postinstall hook runs only after the user can no longer cancel installation.
!macro NSIS_HOOK_POSTINSTALL
  Call LPApplyPostInstall
!macroend

Function un.LPCleanupAssociationsBeforeUninstall
  Push $0
  ClearErrors
  ExecWait '"$INSTDIR\LitematicaPreview.exe" --unregister' $0
  ${If} ${Errors}
    Pop $0
    Abort "无法启动文件关联清理，应用尚未被卸载。"
  ${ElseIf} $0 != 0
    Pop $0
    Abort "无法清理文件关联，应用尚未被卸载。"
  ${EndIf}
  ClearErrors
  Pop $0
FunctionEnd

!macro NSIS_HOOK_PREUNINSTALL
  ; This hook runs before the built-in running-app check.
  ; Check before any registry mutation so a canceled removal leaves the installation untouched.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  Call un.LPCleanupAssociationsBeforeUninstall
!macroend
