#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/dev-up.sh — provisiona o ambiente de desenvolvimento em UM comando.
#
#   1. garante um PostgreSQL 16 + PostGIS 3.4:
#        - com docker:            `docker compose up -d postgis` (healthcheck)
#        - sem docker:            reusa `scripts/test-pg.sh --no-tests` (cluster
#                                 efêmero via conda-forge, porta 55432)
#        - já no ar / TA_PG_DSN:  usa o que existe (não sobe nada de novo)
#   2. `ta-cli doctor`   — relatório do ambiente (informativo neste ponto)
#   3. `ta-cli migrate`  — aplica o schema (idempotente)
#   4. `ta-cli doctor`   — validação final (reprova o script se algo falhar)
#   5. `ta-cli seed-demo`— projeto `demo` com dados sintéticos
#
# Uso:
#   scripts/dev-up.sh            # provisiona, valida e imprime próximos passos
#   scripts/dev-up.sh --gui      # faz o acima e abre a GUI (precisa de display)
#
# Variáveis:
#   TA_PG_DSN     DSN já existente (pula o provisionamento)
#   TA_PG_PORT    porta do cluster conda (default 55432)
#   TA_CLI_BIN    binário do ta-cli (default target/debug/ta-cli)
#
# Idempotente: rodar duas vezes não sobe banco duplicado nem duplica dados do
# seed (o seed resolve camada por nome).
# ---------------------------------------------------------------------------
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

log() { printf '[dev-up] %s\n' "$*"; }
die() { printf '[dev-up] ERRO: %s\n' "$*" >&2; exit 1; }

# Mascara a senha de um DSN para **exibição** (postgres://user:senha@host →
# postgres://user:***@host). DSNs sem senha (ou em formato keyword=value)
# passam intocados. O valor real NUNCA é mascarado nos `export` de ambiente.
mask_dsn() {
    printf '%s' "$1" | sed -E 's#(://[^:/@]+):[^@/]*@#\1:***@#'
}

usage() {
    sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

OPEN_GUI=0
for arg in "$@"; do
    case "$arg" in
        --gui) OPEN_GUI=1 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "argumento desconhecido: $arg (use --gui | --help)" ;;
    esac
done

# cargo vive no PATH de login shell em alguns hosts (mesmo gotcha do test-pg.sh).
export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null 2>&1 || die "cargo não encontrado (instale o rustup em ~/.cargo)"

DOCKER_DSN="postgres://ta:ta@localhost:5432/ta"
CONDA_PORT="${TA_PG_PORT:-55432}"
CONDA_DSN="postgres://ta:ta@127.0.0.1:$CONDA_PORT/ta"

port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

docker_usable() {
    command -v docker >/dev/null 2>&1 || return 1
    docker compose version >/dev/null 2>&1 || return 1
    docker info >/dev/null 2>&1 || return 1
}

wait_for_port() {
    local port="$1" tries="${2:-60}"
    for _ in $(seq 1 "$tries"); do
        if port_open "$port"; then return 0; fi
        sleep 1
    done
    return 1
}

# Espera o healthcheck do container do compose (o nome vem de
# docker-compose.yml: `container_name: ta-postgis`). O healthcheck do compose
# é o `pg_isready` do próprio serviço — sinal melhor que "a porta abriu".
wait_for_health() {
    local container="$1" tries="${2:-60}" status
    for _ in $(seq 1 "$tries"); do
        status="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}' \
            "$container" 2>/dev/null || true)"
        case "$status" in
            healthy) return 0 ;;
            # sem healthcheck declarado: cai no teste de porta do chamador
            none) return 2 ;;
            unhealthy) return 1 ;;
        esac
        sleep 1
    done
    return 1
}

# --- 1) banco ---------------------------------------------------------------
DSN=""
HOW=""
STOP_HINT=""

if [ -n "${TA_PG_DSN:-}" ]; then
    DSN="$TA_PG_DSN"
    HOW="TA_PG_DSN do ambiente"
elif port_open 5432; then
    DSN="$DOCKER_DSN"
    HOW="banco já escutando em 5432"
elif docker_usable; then
    log "docker disponível — subindo o service postgis"
    docker compose up -d postgis
    # healthcheck do serviço (nome fixo em docker-compose.yml); se o compose
    # não declarar healthcheck, cai no teste de porta.
    wait_for_health ta-postgis 60 && health_ok=0 || health_ok=$?
    case "$health_ok" in
        0) log "postgis saudável (healthcheck ok)" ;;
        2) log "container sem healthcheck declarado — conferindo só a porta" ;;
        *) die "postgis ficou unhealthy em 60s (veja: docker compose logs postgis)" ;;
    esac
    wait_for_port 5432 60 || die "porta 5432 não abriu (veja: docker compose logs postgis)"
    DSN="$DOCKER_DSN"
    HOW="docker compose (postgis/postgis:16-3.4)"
elif port_open "$CONDA_PORT"; then
    DSN="$CONDA_DSN"
    HOW="cluster conda já no ar na porta $CONDA_PORT"
else
    log "docker indisponível — provisionando PG16 + PostGIS 3.4 via conda (scripts/test-pg.sh)"
    if ! out="$(bash scripts/test-pg.sh --no-tests 2>&1)"; then
        printf '%s\n' "$out" >&2
        die "o caminho conda (scripts/test-pg.sh) falhou — veja a saída acima"
    fi
    DSN="$(printf '%s\n' "$out" | sed -n 's/^\[test-pg\] DSN: //p' | tail -n1)"
    [ -n "$DSN" ] || {
        printf '%s\n' "$out" >&2
        die "não consegui extrair o DSN da saída do test-pg.sh"
    }
    HOW="cluster conda efêmero (scripts/test-pg.sh --no-tests)"
    STOP_HINT="$(printf '%s\n' "$out" | sed -n 's/^\[test-pg\] para encerrar: //p' | tail -n1 || true)"
fi

export TA_PG_DSN="$DSN"
export TA_DATABASE_URL="$DSN" # a GUI pré-preenche o diálogo de conexão com ela
log "banco: $HOW"
log "dsn:   $(mask_dsn "$DSN")"

# --- 2) CLI -----------------------------------------------------------------
CLI="${TA_CLI_BIN:-$ROOT/target/debug/ta-cli}"
if [ -z "${TA_CLI_BIN:-}" ]; then
    # Build incondicional: cargo não faz nada quando está atualizado, e o
    # script NUNCA deve rodar um binário velho (sources novos → binário novo).
    log "compilando o ta-cli"
    cargo build -p ta-cli --quiet
fi
[ -x "$CLI" ] || die "binário do ta-cli não encontrado em $CLI (rode: cargo build -p ta-cli)"

# --- 3) doctor → migrate → doctor → seed-demo -------------------------------
log "1/4 doctor (informativo — antes do migrate pode acusar migrações pendentes)"
"$CLI" doctor --dsn "$DSN" || log "doctor ainda reprova; o passo de migrate abaixo resolve o schema"
# OBS: o caminho conda sobe um cluster vazio e a extensão postgis; a criação
# da extensão e do schema é feita pelo migrate (e é idempotente).

log "2/4 migrate (schema idempotente)"
"$CLI" migrate --to "$DSN"

log "3/4 doctor (validação)"
"$CLI" doctor --dsn "$DSN" || die "doctor reprovou depois do migrate — corrija o que está acima e rode de novo"

log "4/4 seed-demo (projeto \`demo\` com dados sintéticos)"
"$CLI" seed-demo --dsn "$DSN"

# --- 4) GUI ou instruções ---------------------------------------------------
if [ "$OPEN_GUI" = 1 ]; then
    if [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
        log "abrindo a GUI (TA_DATABASE_URL pré-preenchida; abra o projeto \`demo\` com Ctrl+O)"
        exec cargo run -p ta-gui --quiet
    fi
    log "AVISO: sem DISPLAY/WAYLAND_DISPLAY neste host — não consigo abrir a GUI aqui."
    log "  num host com display: TA_DATABASE_URL='$(mask_dsn "$DSN")' cargo run -p ta-gui"
fi

log "pronto. ambiente validado (doctor exit 0) e projeto \`demo\` semeado."
log "  GUI:  TA_DATABASE_URL='$(mask_dsn "$DSN")' cargo run -p ta-gui   (Conectar → Ctrl+O → \`demo\`)"
if [ -n "$STOP_HINT" ]; then
    log "  banco: $STOP_HINT"
fi
