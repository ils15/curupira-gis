; =============================================================================
; Curupira GIS — instalador Windows (Inno Setup 6)
; =============================================================================
; Compilado pelo release.yml (job build-windows) APÓS o zip portátil:
;   ISCC.exe packaging\windows\curupira.iss /DVERSION=0.1.0 /DBR_ISL=compiler:Languages\<arquivo>.isl
;
; STAGING ESPERADO (montado pelo workflow em dist\installer-stage, mesma
; composição do zip portátil):
;   curupira-gis.exe
;   ta-cli.exe
;   *.dll              (bin completo do triplet vcpkg x64-windows: gdal, proj,
;                       geos, sqlite, expat e deps transitivas)
;   share\gdal\*       (GDAL_DATA)
;   share\proj\*       (PROJ_LIB)
;   README.md LICENSE-MIT LICENSE-APACHE NOTICE.md
;
; ALINHAMENTO COM O main.rs (helper `use_bundled_gdal_data`, crates/ta-gui):
;   o app resolve `<dir-do-exe>\share\proj` (PROJ_LIB) e `<dir-do-exe>\share\gdal`
;   (GDAL_DATA) em runtime, com prioridade p/ variáveis já definidas pelo
;   usuário. Este instalador espelha exatamente o layout do zip: exe em
;   {app} e dados em {app}\share\... — NENHUMA variável de ambiente é
;   gravada no registro (a resolução é só pelo helper do main.rs).
;
; Paths relativos abaixo resolvem contra o diretório deste .iss
; (packaging\windows) — sem SourceDir, sem dependência do CWD do ISCC.
; =============================================================================

#define MyAppName "Curupira GIS"
#define MyAppExe "curupira-gis.exe"
#define MyAppCli "ta-cli.exe"

; /DVERSION=<x.y.z> (numérico pontuado — o workflow tira o sufixo "-alpha" da
; tag; AppVersion numérica é aceita por qualquer Inno Setup 6.x).
#ifndef VERSION
  #error "Compile com /DVERSION=x.y.z (numérico, ex.: 0.1.0)"
#endif

; Tradução pt-BR (opcional): o nome do arquivo .isl varia entre
; distribuições/versões do Inno Setup (BrazilianPortuguese.isl no Inno 6
; oficial; PortugueseBrazil.isl em builds antigos/alternativos). O workflow
; resolve o .isl no Languages\ do ISCC REAL e passa /DBR_ISL; SEM /DBR_ISL o
; instalador compila EN-only (degradação graciosa pós-run #3 — ausência de
; idioma NUNCA deve falhar o job; antes o fallback estático abaixo exigia o
; arquivo mesmo sem /DBR_ISL e quebrava o caminho EN-only).

[Setup]
; AppId FIXO: identifica o app no Adicionar/Remover Programas — não regenerar
; entre releases, senão o Windows trata cada versão como app distinto.
AppId={{70AB6D31-0A0A-40FC-B319-938E7CBA14A2}
AppName={#MyAppName}
AppVersion={#VERSION}
AppVerName={#MyAppName} {#VERSION}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
UninstallDisplayIcon={app}\{#MyAppExe}
LicenseFile=..\..\LICENSE-MIT
OutputDir=..\..\dist
OutputBaseFilename=CurupiraGIS-Setup-x64
; {autopf} + "x64compatible" instala em Program Files (64-bit) — 6.3+;
; PrivilegesRequiredOverridesAllowed permite "instalar só p/ mim" sem admin.
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequiredOverridesAllowed=dialog
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
; ~100 MB de DLLs + dados — o zip portátil usa o mesmo volume (aceito no alpha).

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
#ifdef BR_ISL
Name: "ptbr"; MessagesFile: "{#BR_ISL}"
#endif

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; \
    GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
; Binários (mesma marca do zip: curupira-gis.exe = GUI, ta-cli.exe = CLI)
Source: "..\..\dist\installer-stage\{#MyAppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\installer-stage\{#MyAppCli}"; DestDir: "{app}"; Flags: ignoreversion
; DLLs de runtime do vcpkg (gdal + deps transitivas) — mesma política do zip
Source: "..\..\dist\installer-stage\*.dll"; DestDir: "{app}"; Flags: ignoreversion
; Dados de projeção/GDAL — caminhos EXATAMENTE como o main.rs espera:
;   {app}\share\proj  -> PROJ_LIB   (main.rs: <exe>\share\proj)
;   {app}\share\gdal  -> GDAL_DATA  (main.rs: <exe>\share\gdal)
; recursesubdirs: projdb/proj 9+ e gdal têm subpastas (ex.: proj\projdb,
; gdal\plugins) — createallsubdirs cria vazias também.
Source: "..\..\dist\installer-stage\share\proj\*"; DestDir: "{app}\share\proj"; \
    Flags: ignoreversion recursesubdirs createallsubdirs
Source: "..\..\dist\installer-stage\share\gdal\*"; DestDir: "{app}\share\gdal"; \
    Flags: ignoreversion recursesubdirs createallsubdirs
; Documentação
Source: "..\..\dist\installer-stage\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\installer-stage\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\installer-stage\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\dist\installer-stage\NOTICE.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"
Name: "{group}\LICENSE (MIT)"; Filename: "{app}\LICENSE-MIT"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExe}"; Description: "{cm:LaunchProgram,Curupira GIS}"; \
    Flags: nowait postinstall skipifsilent
