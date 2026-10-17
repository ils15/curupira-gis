#!/usr/bin/env bash
# =============================================================================
# Curupira GIS — bundle macOS (CurupiraGIS.app + zip NÃO ASSINADO)
# =============================================================================
# Uso (o release.yml chama exatamente assim, a partir da raiz do repo):
#   bash packaging/macos/bundle.sh <versão> <dir-dos-binários> <dir-de-saída>
#   bash packaging/macos/bundle.sh 0.1.0 target/release dist
#
# O que faz:
#   1. Monta CurupiraGIS.app/Contents/{MacOS,Resources,Frameworks} + Info.plist
#   2. Copia curupira-gis (GUI) e ta-cli (CLI) para Contents/MacOS
#   3. Walk recursivo de `otool -L`: copia TODA dylib não-sistema (fora
#      /usr/lib e /System), RESOLVENDO cada dependência p/ um arquivo real —
#      caminho absoluto (brew clássico) OU @rpath/<base> resolvido contra
#      LC_RPATH do referenciador, $GDAL_HOME/lib (conda-forge) e prefixos
#      brew/usr — para Contents/Frameworks
#   4. Reescreve os install names: cada dependência ABSOLUTA vira
#      @rpath/<basename> via `install_name_tool -change` (-id nas copiadas);
#      dependências já-@rpath (conda) mantêm o nome — o basename em
#      Frameworks é o mesmo e o LC_RPATH do executável resolve. Executáveis
#      ganham LC_RPATH @executable_path/../Frameworks
#   5. Re-assina ad-hoc (codesign -f -s -) tudo que foi alterado — OBRIGATÓRIO
#      no arm64: o linker assina ad-hoc na build, mas install_name_tool
#      INVALIDA a assinatura e o macOS se recusa a carregar o binário
#      ("code signature invalid") sem re-assinatura.
#   6. Copia dados de runtime GDAL/PROJ do prefixo do build ($GDAL_HOME/share,
#      layout conda) p/ Contents/MacOS/share — o ta-gui (main.rs) aponta
#      PROJ_LIB/GDAL_DATA p/ <dir-do-exe>/share quando existir
#   7. Zipa com ditto (CurupiraGIS-macos-arm64.zip)
#
# DECISÃO (não usar DYLD_LIBRARY_PATH): wrapper com DYLD_* é frágil (variáveis
# DYLD_* são descartadas em vários cenários de spawn e o .app fica dependente
# do launcher); @rpath+Frameworks é o layout canônico de bundle. Idempotente:
# recria o bundle do zero a cada execução (rm -rf antes de montar).
#
# PREMISSAS (valem p/ brew clássico E p/ conda-forge 3.12.x, run #4):
#   - brew: dylibs referenciam-se por caminhos ABSOLUTOS
#     (/opt/homebrew/opt/...) — nunca por @rpath entre si;
#   - conda-forge: dylibs têm install name @rpath/<basename> (verificado na
#     libgdal-core 3.12.4 osx-arm64: LC_ID_DYLIB=@rpath/libgdal.38.dylib e 32
#     deps @rpath) — o resolver abaixo mapeia cada nome p/ o arquivo real em
#     $GDAL_HOME/lib e o bundle regrava tudo como @rpath/<basename>;
#   - basenames de dylib são únicos entre os prefixes (brew e env conda).
# =============================================================================

set -euo pipefail

VERSION="${1:?uso: bundle.sh <versão> <dir-dos-binários> <dir-de-saída>}"
BIN_DIR="${2:?falta <dir-dos-binários>}"
OUT_DIR="${3:?falta <dir-de-saída>}"

APP="CurupiraGIS.app"
CONTENTS="$OUT_DIR/$APP/Contents"
MACOS="$CONTENTS/MacOS"
FRAMEWORKS="$CONTENTS/Frameworks"
RESOURCES="$CONTENTS/Resources"
ZIP="$OUT_DIR/CurupiraGIS-macos-arm64.zip"
RPATH="@executable_path/../Frameworks"

for tool in otool install_name_tool codesign ditto; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "ERRO: '$tool' não encontrado — este script roda no macOS (runner)." >&2
    exit 1
  }
done
[[ -x "$BIN_DIR/curupira-gis" ]] || { echo "ERRO: $BIN_DIR/curupira-gis não existe (build antes)." >&2; exit 1; }
[[ -x "$BIN_DIR/ta-cli" ]] || { echo "ERRO: $BIN_DIR/ta-cli não existe (build antes)." >&2; exit 1; }

# CWD-independente: resolve a raiz do repo a partir deste arquivo; os docs
# (README/LICENSE) e args relativos (target/release, dist) ancoram aqui.
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../.."

# --- monta o esqueleto do bundle (idempotente: sempre do zero) ---------------
rm -rf "$OUT_DIR/$APP" "$ZIP"
mkdir -p "$MACOS" "$FRAMEWORKS" "$RESOURCES"

cp "$BIN_DIR/curupira-gis" "$MACOS/"
cp "$BIN_DIR/ta-cli" "$MACOS/"
cp README.md LICENSE-MIT LICENSE-APACHE NOTICE.md "$RESOURCES/"

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>Curupira GIS</string>
	<key>CFBundleDisplayName</key><string>Curupira GIS</string>
	<key>CFBundleIdentifier</key><string>io.github.ils15.curupira-gis</string>
	<key>CFBundleExecutable</key><string>curupira-gis</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>${VERSION}</string>
	<key>CFBundleVersion</key><string>${VERSION}</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
	<key>NSHighResolutionCapable</key><true/>
	<key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

# --- walk de dylibs -----------------------------------------------------------
# basenames já copiados para Frameworks (evita copiar/re-processar duas vezes)
declare -A COPIED=()

# Dependências de um binário/dylib: uma por linha, no formato do `otool -L`:
#   "\t<caminho> (compatibility version X, current version Y)"
# A 1ª linha de uma DYLIB é o próprio id (após -id, vira @rpath/... e é
# ignorada pelo case abaixo). A linha de cabeçalho "<arquivo>:" não casa.
list_deps() {
  otool -L "$1" | sed -n 's/^[[:space:]]*\([^[:space:]]*\)[[:space:]]*(compatibility version.*/\1/p'
}

# LC_RPATH de um binário/dylib (otool -l): linhas `path <valor>` dentro de
# blocos `cmd LC_RPATH`.
list_rpaths() {
  otool -l "$1" | awk '/cmd LC_RPATH/ { inrp = 1; next } inrp && $1 == "path" { print $2; inrp = 0 }'
}

# Resolve uma entrada do `otool -L` p/ um caminho REAL no disco.
#   - caminho absoluto: devolve como está (brew clássico, libs externas);
#   - @rpath/X / @loader_path/X / @executable_path/X: procura <X> na ordem
#       (1) LC_RPATH do referenciador (@executable_path/@loader_path →
#           dirname do referenciador durante o walk);
#       (2) $GDAL_HOME/lib — layout conda-forge: TODAS as dylibs do env
#           ficam nesse único dir (run #4: libgdal.38 e as ~32 deps);
#       (3) prefixos brew/usr locais (dev fora de CI, brew atual usa
#           absolutos — fica como redundância).
# Imprime o caminho resolvido (1 linha) ou retorna 1 (chamador decide).
resolve_dep() {
  local dep="$1" ref="$2" base cand r
  case "$dep" in
    @rpath/*|@loader_path/*|@executable_path/*) base="${dep##*/}" ;;
    *) printf '%s\n' "$dep"; return 0 ;;
  esac
  while IFS= read -r r; do
    [ -n "$r" ] || continue
    cand="${r//@executable_path/$(dirname "$ref")}"
    cand="${cand//@loader_path/$(dirname "$ref")}"
    [ -f "$cand/$base" ] && { printf '%s\n' "$cand/$base"; return 0; }
  done < <(list_rpaths "$ref")
  for cand in "${GDAL_HOME:-}/lib" /opt/homebrew/lib /usr/local/lib; do
    [ -n "$cand" ] && [ -f "$cand/$base" ] && { printf '%s\n' "$cand/$base"; return 0; }
  done
  return 1
}

# Copia $dep (caminho JÁ RESOLVIDO por resolve_dep) p/ Frameworks (se ainda
# não copiada), ajusta o -id e reprocessa as DEPENDÊNCIAS DELA.
copy_dylib() {
  local dep="$1" base
  base="$(basename "$dep")"
  [[ -n "${COPIED[$base]:-}" ]] && return 0
  cp -L "$dep" "$FRAMEWORKS/$base"
  COPIED["$base"]=1
  install_name_tool -id "@rpath/$base" "$FRAMEWORKS/$base"
  fix_deps "$FRAMEWORKS/$base"
}

# Em $1 (alvo), resolve cada dependência não-sistema, copia-a p/ Frameworks e
# regrava referências absolutas p/ @rpath/<basename>.
#   - dependências @rpath/... (conda): NÃO levam -change — o nome já é
#     @rpath/<basename> e o basename em Frameworks é o mesmo; quem resolve em
#     runtime é o LC_RPATH @executable_path/../Frameworks do executável
#     (dyld usa os rpaths do executável p/ TODAS as imagens do link chain);
#   - dependências absolutas (brew): -change p/ @rpath/<basename>.
fix_deps() {
  local target="$1" dep resolved base
  while IFS= read -r dep; do
    case "$dep" in
      /usr/lib/*|/System/*) continue ;;  # dylibs do sistema macOS: nunca copiar
    esac
    resolved="$(resolve_dep "$dep" "$target")" || {
      echo "ERRO: dependência '$dep' (referenciada por '$target') não resolvida." >&2
      echo "      Origens testadas: LC_RPATH do referenciador, \$GDAL_HOME/lib" >&2
      echo "      (conda-forge), /opt/homebrew/lib, /usr/local/lib." >&2
      exit 1
    }
    base="$(basename "$resolved")"
    copy_dylib "$resolved"
    case "$dep" in
      @rpath/*) ;;  # já relativa e basename idêntico em Frameworks — no-op
      *) install_name_tool -change "$dep" "@rpath/$base" "$target" ;;
    esac
  done < <(list_deps "$target")
}

for exe in "$MACOS/curupira-gis" "$MACOS/ta-cli"; do
  fix_deps "$exe"
  # LC_RPATH p/ resolver @rpath: o bundle é novo — o rpath nunca pré-existe
  # (um segundo -add_rpath idêntico daria erro, mas o rm -rf acima garante
  # execução única por run).
  install_name_tool -add_rpath "$RPATH" "$exe"
done

# --- dados de runtime GDAL/PROJ (consumidos por ta-gui/src/main.rs) ----------
# main.rs aponta PROJ_LIB/GDAL_DATA p/ <dir-do-exe>/share/{proj,gdal} quando
# existirem; no bundle, dir-do-exe = Contents/MacOS. Origem: o MESMO prefixo
# usado no build ($GDAL_HOME, layout conda). Sem $GDAL_HOME (dev com brew) →
# skip: comportamento idêntico ao Linux (sem dados bundled, PROJ/GDAL do
# sistema). Falha na cópia = erro (set -e), nunca bundle pela metade.
if [ -n "${GDAL_HOME:-}" ]; then
  for d in proj gdal; do
    if [ -d "$GDAL_HOME/share/$d" ]; then
      mkdir -p "$MACOS/share"
      cp -R "$GDAL_HOME/share/$d" "$MACOS/share/$d"
      echo "dados bundled: $MACOS/share/$d <- $GDAL_HOME/share/$d"
    fi
  done
fi

# --- re-assinatura ad-hoc (obrigatória pós-install_name_tool no arm64) -------
while IFS= read -r dylib; do
  codesign --force --sign - "$dylib"
done < <(find "$FRAMEWORKS" -type f -name "*.dylib")
codesign --force --sign - "$MACOS/curupira-gis"
codesign --force --sign - "$MACOS/ta-cli"

# --- zip (ditto preserva metadados; -k = pkzip) ------------------------------
ditto -c -k --sequesterRsrc --keepParent "$OUT_DIR/$APP" "$ZIP"

echo "OK: bundle $OUT_DIR/$APP"
echo "OK: zip    $ZIP"
