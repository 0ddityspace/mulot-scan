; Installateur Windows de Mulot-scan (NSIS 3).
;
; Généré par `make windows-installer` ou par le workflow de release :
;   makensis -DVERSION=0.1.0 -DEXEFILE=chemin/mulot-scan.exe -DROOT=racine-du-dépôt \
;            -DOUTFILE=dist/mulot-scan-0.1.0-windows-x86_64-setup.exe [-DSIGN_CMD="…"] installer.nsi
;
; Avec SIGN_CMD, l'installateur et le programme de désinstallation sont signés
; (Authenticode) au moment de leur création ; « %1 » est remplacé par le fichier.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "x64.nsh"
!include "FileFunc.nsh"

!ifndef VERSION
  !error "VERSION non défini (voir l'en-tête)"
!endif
!ifndef EXEFILE
  !error "EXEFILE non défini (voir l'en-tête)"
!endif
!ifndef ROOT
  !error "ROOT non défini (voir l'en-tête)"
!endif
!ifndef OUTFILE
  !error "OUTFILE non défini (voir l'en-tête)"
!endif

!define APP "Mulot-scan"
!define EXE "mulot-scan.exe"
!define PUBLISHER "Mulot-scan"
!define URL "https://github.com/0ddityspace/mulot-scan"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"

!ifdef SIGN_CMD
  !finalize '${SIGN_CMD} "%1"'
  !uninstfinalize '${SIGN_CMD} "%1"'
!endif

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\${APP}"
InstallDirRegKey HKLM "${UNINST_KEY}" "InstallLocation"
RequestExecutionLevel admin
BrandingText "${APP} ${VERSION}"

; ---------------------------------------------------------------- pages

!define MUI_ICON "${ROOT}/assets/icon.ico"
!define MUI_UNICON "${ROOT}/assets/icon.ico"
!define MUI_ABORTWARNING
!define MUI_COMPONENTSPAGE_SMALLDESC
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_LINK "$(LinkTesseract)"
!define MUI_FINISHPAGE_LINK_LOCATION "https://github.com/UB-Mannheim/tesseract/wiki"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; La langue du système est choisie automatiquement.
!insertmacro MUI_LANGUAGE "French"
!insertmacro MUI_LANGUAGE "English"

LangString SecApp ${LANG_FRENCH} "Mulot-scan"
LangString SecApp ${LANG_ENGLISH} "Mulot-scan"
LangString SecAppDesc ${LANG_FRENCH} "Le programme et son raccourci dans le menu Démarrer."
LangString SecAppDesc ${LANG_ENGLISH} "The program and its Start menu shortcut."
LangString SecDesktop ${LANG_FRENCH} "Raccourci sur le Bureau"
LangString SecDesktop ${LANG_ENGLISH} "Desktop shortcut"
LangString SecDesktopDesc ${LANG_FRENCH} "Ajoute une icône Mulot-scan sur le Bureau."
LangString SecDesktopDesc ${LANG_ENGLISH} "Adds a Mulot-scan icon to the desktop."
LangString LinkTesseract ${LANG_FRENCH} "Installer Tesseract pour la reconnaissance de texte (OCR)"
LangString LinkTesseract ${LANG_ENGLISH} "Install Tesseract for text recognition (OCR)"
LangString Need64 ${LANG_FRENCH} "Mulot-scan nécessite une version 64 bits de Windows."
LangString Need64 ${LANG_ENGLISH} "Mulot-scan requires a 64-bit version of Windows."

VIProductVersion "${VERSION}.0"
VIFileVersion "${VERSION}.0"
VIAddVersionKey /LANG=${LANG_FRENCH} "ProductName" "${APP}"
VIAddVersionKey /LANG=${LANG_FRENCH} "CompanyName" "${PUBLISHER}"
VIAddVersionKey /LANG=${LANG_FRENCH} "FileDescription" "Installation de ${APP}"
VIAddVersionKey /LANG=${LANG_FRENCH} "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=${LANG_FRENCH} "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=${LANG_FRENCH} "LegalCopyright" "Licence MIT ou Apache 2.0"

; ---------------------------------------------------------------- installation

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_OK|MB_ICONSTOP "$(Need64)"
    Abort
  ${EndIf}
  SetRegView 64
  SetShellVarContext all
FunctionEnd

Section "!$(SecApp)" SecApp
  SectionIn RO
  SetOutPath "$INSTDIR"
  File "/oname=${EXE}" "${EXEFILE}"
  File "${ROOT}/README.md"
  File "${ROOT}/LICENSE-MIT"
  File "${ROOT}/LICENSE-APACHE"
  SetOutPath "$INSTDIR\docs"
  File "${ROOT}/docs/PROTOCOLE.md"
  SetOutPath "$INSTDIR"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\${EXE}"

  ; Entrée dans « Applications installées ».
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINST_KEY}" "URLInfoAbout" "${URL}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKLM "${UNINST_KEY}" "EstimatedSize" $0
SectionEnd

Section "$(SecDesktop)" SecDesktop
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\${EXE}"
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SecApp} "$(SecAppDesc)"
  !insertmacro MUI_DESCRIPTION_TEXT ${SecDesktop} "$(SecDesktopDesc)"
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ---------------------------------------------------------------- désinstallation

Function un.onInit
  SetRegView 64
  SetShellVarContext all
FunctionEnd

; Les réglages et le calibrage de l'utilisateur (dossier AppData) sont conservés.
Section "Uninstall"
  Delete "$INSTDIR\${EXE}"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\LICENSE-MIT"
  Delete "$INSTDIR\LICENSE-APACHE"
  Delete "$INSTDIR\docs\PROTOCOLE.md"
  RMDir "$INSTDIR\docs"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKLM "${UNINST_KEY}"
SectionEnd
