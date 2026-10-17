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
#      /usr/lib e /System — ou seja, tudo do Homebrew) para Contents/Frameworks
#   4. Reescreve os install names: cada dependência vira @rpath/<basename>,
#      via `install_name_tool -change` (-id nas copiadas); os executáveis
#      ganham LC_RPATH @executable_path/../Frameworks
#   5. Re-assina ad-hoc (codesign -f -s -) tudo que foi alterado — OBRIGATÓRIO
#      no arm64: o linker assina ad-hoc na build, mas install_name_tool
#      INVALIDA a assinatura e o macOS se recusa a carregar o binário
#      ("code signature invalid") sem re-assinatura.
#   6. Zipa com ditto (CurupiraGIS-macos-arm64.zip)
#
# DECISÃO (não usar DYLD_LIBRARY_PATH): wrapper com DYLD_* é frágil (variáveis
# DYLD_* são descartadas em vários cenários de spawn e o .app fica dependente
# do launcher); @rpath+Frameworks é o layout canônico de bundle. Idempotente:
# recria o bundle do zero a cada execução (rm -rf antes de montar).
#
# PREMISSAS (válidas no ecossistema Homebrew):
#   - dylibs do brew referenciam-se por caminhos ABSOLUTOS
#     (/opt/homebrew/opt/...) — nunca por @rpath entre si;
#   - basenames de dylib são únicos entre os prefixes do brew (um
#     libfoo.N.dylib só existe em um lugar).
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

# Copia $dep p/ Frameworks (se ainda não copiada), ajusta o -id e reprocessa
# as DEPENDÊNCIAS DELA (a cópia em Frameworks ainda aponta p/ /opt/homebrew).
copy_dylib() {
  local dep="$1" base
  base="$(basename "$dep")"
  [[ -n "${COPIED[$base]:-}" ]] && return 0
  cp -L "$dep" "$FRAMEWORKS/$base"
  COPIED["$base"]=1
  install_name_tool -id "@rpath/$base" "$FRAMEWORKS/$base"
  fix_deps "$FRAMEWORKS/$base"
}

# Em $1 (alvo), reescreve cada dependência não-sistema p/ @rpath/<basename>.
fix_deps() {
  local target="$1" dep
  while IFS= read -r dep; do
    case "$dep" in
      @*) continue ;;        # @rpath/@loader_path/@executable_path: já relativa
      /usr/lib/*|/System/*) continue ;;  # dylibs do sistema macOS: nunca copiar
    esac
    copy_dylib "$dep"
    install_name_tool -change "$dep" "@rpath/$(basename "$dep")" "$target"
  done < <(list_deps "$target")
}

for exe in "$MACOS/curupira-gis" "$MACOS/ta-cli"; do
  fix_deps "$exe"
  # LC_RPATH p/ resolver @rpath: o bundle é novo — o rpath nunca pré-existe
  # (um segundo -add_rpath idêntico daria erro, mas o rm -rf acima garante
  # execução única por run).
  install_name_tool -add_rpath "$RPATH" "$exe"
done

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
