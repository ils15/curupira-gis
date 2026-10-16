#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# scripts/test-pg.sh — cluster PostgreSQL 16 + PostGIS 3.4 EFÊMERO no $HOME.
#
# Contexto do host (ver DISCOVERY.md): sem docker/podman e com sudo
# bloqueado, o único caminho viável é conda-forge no diretório do usuário.
# Pins alinhados com docker-compose.yml (postgis/postgis:16-3.4):
#   postgresql=16 + postgis=3.4 (mesma major PG, mesma minor PostGIS).
# NOTA: pins EXATOS (16.15/3.4.3, DISCOVERY) não resolvem juntos no
# conda-forge atual (conflito icu/libxml2); com as constraints de
# major/minor o solver escolheu 16.10 + 3.4.x — mesmo contrato R7.
#
# Uso:
#   scripts/test-pg.sh              # sobe o cluster, aplica migrações e roda
#                                   # a suíte gated (TA_TEST_PG=1), depois derruba
#   scripts/test-pg.sh --no-tests   # só sobe o cluster e imprime o DSN
#                                   # (cluster fica no ar; pare com o comando
#                                   #  impresso no final)
#
# Variáveis:
#   TA_PG_PORT (default 55432) · TA_PG_DSN (imposto pelo script)
#   TA_PG_ENV  (default ta-pg16, criado em $HOME/.conda/envs/)
#
# A criação do env conda é a ÚNICA instalação do workflow (decisão Gate 1);
# todo o resto roda em diretórios temporários e é descartado no fim.
# ---------------------------------------------------------------------------
set -euo pipefail

PORT="${TA_PG_PORT:-55432}"
ENV_DIR="${TA_PG_ENV_DIR:-$HOME/.conda/envs/${TA_PG_ENV:-ta-pg16}}"
BASE_TMP="${TMPDIR:-/tmp/opencode}"

log() { printf '[test-pg] %s\n' "$*"; }
die() { printf '[test-pg] ERRO: %s\n' "$*" >&2; exit 1; }

# --- toolchain: cargo vive só no PATH de login shell (risco R4) -------------
export PATH="$HOME/.cargo/bin:$PATH"
command -v cargo >/dev/null || die "cargo não encontrado (rustup instalado em ~/.cargo?)"

# --- 1) env conda (cria só se não existir) ----------------------------------
if [ ! -x "$ENV_DIR/bin/initdb" ]; then
    log "env conda ausente em $ENV_DIR — criando (única instalação do workflow)"
    if command -v mamba >/dev/null; then
        PKG=mamba
    elif command -v conda >/dev/null; then
        PKG=conda
    else
        die "nem mamba nem conda no PATH; impossível provisionar PG16+PostGIS"
    fi
    log "usando $PKG create (pode baixar ~200 MB do conda-forge)"
    "$PKG" create -y -p "$ENV_DIR" -c conda-forge postgresql=16 postgis=3.4 \
        || die "criação do env conda falhou (rede/catálogo) — testes PG ficam [UNKNOWN]; a suíte unitária continua verde sem banco"
else
    log "env conda existente: $ENV_DIR"
fi

BINDIR="$ENV_DIR/bin"

# --- 2) dirs efêmeros --------------------------------------------------------
PGDATA="$(mktemp -d "$BASE_TMP/ta-pgdata-XXXXXX")"
SOCKDIR="$(mktemp -d "$BASE_TMP/ta-pgsock-XXXXXX")"
PGLOG="$SOCKDIR/postgres.log"

cleanup() {
    set +e
    if [ -n "${PGDATA:-}" ] && [ -f "$PGDATA/PG_VERSION" ]; then
        log "parando cluster e limpando $PGDATA"
        "$BINDIR/pg_ctl" -D "$PGDATA" stop -m fast >/dev/null 2>&1
        rm -rf "$PGDATA" "$SOCKDIR"
    fi
}
trap cleanup EXIT

# --- 3) initdb + start -------------------------------------------------------
if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then
    exec 3>&- 3<&- || true
    die "porta $PORT já está em uso (encerre o serviço ou mude TA_PG_PORT)"
fi
log "initdb (usuário ta, trust local)"
"$BINDIR/initdb" -D "$PGDATA" -U ta -A trust -E UTF8 >/dev/null

log "subindo PostgreSQL na porta $PORT"
"$BINDIR/pg_ctl" -D "$PGDATA" \
    -o "-p $PORT -k $SOCKDIR -c listen_addresses=127.0.0.1" \
    -l "$PGLOG" -w start >/dev/null

"$BINDIR/createdb" -h 127.0.0.1 -p "$PORT" -U ta ta

log "CREATE EXTENSION postgis"
"$BINDIR/psql" -h 127.0.0.1 -p "$PORT" -U ta -d ta -v ON_ERROR_STOP=1 \
    -c "CREATE EXTENSION IF NOT EXISTS postgis;" >/dev/null

# --- 4) DSN e migrações ------------------------------------------------------
export TA_PG_DSN="postgres://ta:ta@127.0.0.1:$PORT/ta"
log "DSN: $TA_PG_DSN"

if [ "${1:-}" = "--no-tests" ]; then
    log "cluster no ar. Para testar manualmente:"
    log "  TA_TEST_PG=1 TA_PG_DSN=$TA_PG_DSN cargo test --workspace"
    log "para encerrar: '$BINDIR/pg_ctl -D $PGDATA stop -m fast'"
    trap - EXIT   # mantém o cluster de pé sob pedido explícito
    exit 0
fi

# Migrações + suíte gated: os testes aplicam `sqlx migrate` (via Migrator
# runtime de ta-db::spatial::run_migrations) num banco novo e no banco principal.
# (sqlx-cli não existe neste host; o Migrator usa a MESMA tabela _sqlx_migrations.)
log "rodando suíte gated: TA_TEST_PG=1 cargo test --workspace"
TA_TEST_PG=1 TA_PG_DSN="$TA_PG_DSN" cargo test --workspace
log "OK — suíte de integração verde"
