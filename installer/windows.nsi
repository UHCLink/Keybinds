; Installateur NSIS du composant utilisateur UHC-Link.
; Ce script possède toute la logique d'installation, copie des exécutables,
; démarrage facultatif et enregistrement dans les applications Windows.
; Le runtime Rust ne réexécute aucune de ces opérations quand il démarre.
; Éditer ce fichier canonique, puis make companion, les EXE de dist sont générés.
;
; VERSION, PAYLOAD et OUTPUT sont fournis par scripts/build-release.py.
; PAYLOAD désigne le runtime déjà signé si la signature est configurée.
Unicode true
Target amd64-unicode
; Installation pour l'utilisateur courant, pas de demande d'élévation UAC.
RequestExecutionLevel user
; Compression du paquet d'installation uniquement: le runtime extrait est un PE
; normal, sans décompresseur ajouté à son démarrage.
SetCompressor /SOLID lzma
SetDatablockOptimize on
SetDateSave off
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "Sections.nsh"

!ifdef SIGN_UNINSTALLER_COMMAND
  ; NSIS appelle ce hook avant d'embarquer son désinstalleur. Le script conserve
  ; une copie exacte d'Uninstall.exe après signature pour les vérifications.
  !uninstfinalize '"${SIGN_UNINSTALLER_COMMAND}" "%1"' = 0
!endif

!ifndef VERSION
  !error "VERSION is required"
!endif
!ifndef PAYLOAD
  !error "PAYLOAD is required"
!endif
!ifndef OUTPUT
  !error "OUTPUT is required"
!endif

!define PRODUCT_NAME "UHC-Link Raccourcis"
; Même identité dans la fenêtre, les propriétés du fichier et le registre Windows.
Name "${PRODUCT_NAME}"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\UHC-Link"
; Réutiliser le chemin d'une installation EXE précédente, s'il existe.
InstallDirRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "InstallLocation"
VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1036 "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey /LANG=1036 "CompanyName" "UHC-Link"
VIAddVersionKey /LANG=1036 "FileDescription" "Installation des raccourcis UHC-Link"
VIAddVersionKey /LANG=1036 "FileVersion" "${VERSION}.0"
VIAddVersionKey /LANG=1036 "ProductVersion" "${VERSION}.0"
VIAddVersionKey /LANG=1036 "LegalCopyright" "UHC-Link"

!define MUI_ICON "../resources/app.ico"
!define MUI_UNICON "../resources/app.ico"
!define MUI_WELCOMEPAGE_TITLE "Installer les raccourcis UHC-Link"
!define MUI_WELCOMEPAGE_TEXT "Ce composant permet de configurer des raccourcis pour UHC-Link, même lorsque Minecraft est au premier plan.$\r$\n$\r$\nUne fois le composant installé, les raccourcis se configurent sur le site.$\r$\n$\r$\nCe composant tournera en fond, avec un coût très minimal en performance."
; La dernière page permet de lancer le composant. Le mode --background évite une
; seconde boîte d'information après l'assistant, il ne relance pas l'installation.
!define MUI_FINISHPAGE_RUN "$INSTDIR\UHC-Link-Raccourcis.exe"
!define MUI_FINISHPAGE_RUN_PARAMETERS "--background"
!define MUI_FINISHPAGE_RUN_TEXT "Lancer le composant des raccourcis UHC-Link"
!define MUI_FINISHPAGE_TEXT "Le composant des raccourcis a bien été installé !$\r$\n$\r$\nRetournez sur le site, puis ouvrez les réglages des raccourcis pour configurer vos touches."
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "French"

Section "Composant de raccourcis (obligatoire)" Main
  ; Section obligatoire, les chemins et le registre concernent le compte courant.
  SectionIn RO
  SetShellVarContext current
  SetRegView 64
  SetOutPath "$INSTDIR"
  File /oname=UHC-Link-Raccourcis.exe "${PAYLOAD}"
  ; Une mise à jour remplace l'ancien nom sans effacer settings.json.
  Delete "$INSTDIR\UHC-Link-Keybinds.exe"
  File "../LICENSE"
  File "../THIRD-PARTY-LICENSES.txt"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  ; Enregistrement standard exploité par Paramètres > Applications. NoModify et
  ; NoRepair retirent ces deux fonctions non implémentées, pas la désinstallation.
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "Publisher" "UHC-Link"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "DisplayIcon" '$\"$INSTDIR\UHC-Link-Raccourcis.exe$\",0'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "NoRepair" 1
  Delete "$SMPROGRAMS\UHC-Link Keybinds.lnk"
  ; Nettoyage d'un raccourci d'anciennes versions, aucun nouveau raccourci créé.
SectionEnd

Section "Démarrer avec Windows" Startup
  ; Option visible et désélectionnable, limitée à la clé Run de l'utilisateur.
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "UHC-Link" '$\"$INSTDIR\UHC-Link-Raccourcis.exe$\" --background'
SectionEnd

Section "-ApplyStartupChoice"
  ; Retirer l'ancienne entrée si l'option est décochée pendant une mise à jour.
  ${IfNot} ${SectionIsSelected} ${Startup}
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "UHC-Link"
  ${EndIf}
SectionEnd

Function .onInit
  SetRegView 64
  FindWindow $0 "UHCLinkLocalShortcuts"
  ; Éviter d'écraser le binaire en cours d'exécution. Le joueur peut le quitter
  ; depuis l'icône de notification, l'installateur ne le remplace pas en force.
  ${If} $0 != 0
    IfSilent +2
    MessageBox MB_OK|MB_ICONINFORMATION "Quittez d’abord le composant UHC-Link depuis son icône dans la zone de notification, puis relancez l’installation."
    SetErrorLevel 2
    Quit
  ${EndIf}
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link" "DisplayVersion"
  ; Une mise à jour respecte un démarrage déjà désactivé. Les préférences JSON
  ; ne sont pas supprimées par l'installation ni par cette vérification.
  ${If} $0 != ""
    ReadRegStr $1 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "UHC-Link"
    ${If} $1 == ""
      !insertmacro UnselectSection ${Startup}
    ${EndIf}
  ${EndIf}
FunctionEnd

Section "Uninstall"
  ; Section exécutée après confirmation (ou /S explicitement demandé à NSIS).
  ; Le menu Rust ouvre cet assistant normalement, sans demander /S.
  SetShellVarContext current
  SetRegView 64
  FindWindow $0 "UHCLinkLocalShortcuts"
  ${If} $0 != 0
    ; WM_CLOSE suit le même chemin que Quitter: retrait de l'icône et arrêt API.
    SendMessage $0 0x0010 0 0 /TIMEOUT=3000
    Sleep 300
  ${EndIf}
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "UHC-Link"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\UHC-Link"
  Delete "$SMPROGRAMS\UHC-Link Keybinds.lnk"
  Delete "$INSTDIR\UHC-Link-Raccourcis.exe"
  Delete "$INSTDIR\UHC-Link-Keybinds.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD-PARTY-LICENSES.txt"
  Delete "$INSTDIR\settings.json"
  ; Une désinstallation enlève les préférences, contrairement à une mise à jour.
  Delete "$INSTDIR\settings.next"
  ; Le runtime conserve ses réglages ici même avec un dossier EXE personnalisé.
  Delete "$LOCALAPPDATA\UHC-Link\settings.json"
  Delete "$LOCALAPPDATA\UHC-Link\settings.next"
  RMDir "$LOCALAPPDATA\UHC-Link"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  ; RMDir non récursif: ne pas supprimer des fichiers étrangers au composant.
SectionEnd
