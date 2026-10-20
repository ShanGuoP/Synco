; 整理软件用此固定注册表项定位 Synco；接口端口仍以运行时发现文件为准。
!macro NSIS_HOOK_POSTINSTALL
  WriteRegStr HKCU "Software\Synco\Interop" "Executable" "$INSTDIR\synco-desktop.exe"
  WriteRegDWORD HKCU "Software\Synco\Interop" "Protocol" 1
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ReadRegStr $0 HKCU "Software\Synco\Interop" "Executable"
  StrCmp $0 "$INSTDIR\synco-desktop.exe" 0 +2
    DeleteRegKey HKCU "Software\Synco\Interop"
!macroend
