#!/usr/bin/env bash
# Teste LOCAL do scripts/wait-for-ci.sh com respostas SIMULADAS da API do
# GitHub (um `gh` falso no PATH). Roda no CI (job `release-scripts` do
# ci.yml) — são os cenários do gate que decide se a release é montada.
#
# Uso: bash scripts/test-wait-for-ci.sh
set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_DIR}/scripts/wait-for-ci.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

SHA="deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"

# ── `gh` falso: devolve o JSON conforme o modo e o contador ────────────────
cat >"${TMP}/gh" <<'FAKE'
#!/usr/bin/env bash
# args: api "repos/OWNER/REPO/actions/runs?head_sha=..."  (o script usa gh api)
url="${*: -1}"
counter_file="${FAKE_GH_COUNTER:-/tmp/.fake-gh-counter}"
n=0
[ -f "$counter_file" ] && n="$(cat "$counter_file")"
n=$((n + 1))
echo "$n" >"$counter_file"

emit() { printf '%s' "$1"; exit 0; }

case "${FAKE_GH_MODE:-success}" in
  success)
    emit '{"workflow_runs":[{"id":1,"name":"ci","path":".github/workflows/ci.yml","event":"push","status":"completed","conclusion":"success","created_at":"2026-10-19T10:00:00Z","head_sha":"'$SHA'"}]}'
    ;;
  progress)
    # 1ª chamada: in_progress; 2ª em diante: completed/success
    if [ "$n" -le 1 ]; then
      emit '{"workflow_runs":[{"id":1,"name":"ci","path":".github/workflows/ci.yml","event":"push","status":"in_progress","conclusion":null,"created_at":"2026-10-19T10:00:00Z","head_sha":"'$SHA'"}]}'
    else
      emit '{"workflow_runs":[{"id":1,"name":"ci","path":".github/workflows/ci.yml","event":"push","status":"completed","conclusion":"success","created_at":"2026-10-19T10:00:00Z","head_sha":"'$SHA'"}]}'
    fi
    ;;
  failure)
    emit '{"workflow_runs":[{"id":1,"name":"ci","path":".github/workflows/ci.yml","event":"push","status":"completed","conclusion":"failure","created_at":"2026-10-19T10:00:00Z","head_sha":"'$SHA'"}]}'
    ;;
  missing)
    emit '{"workflow_runs":[]}'
    ;;
  other)
    # run de OUTRO workflow (ex.: release) — não deve ser considerado
    emit '{"workflow_runs":[{"id":9,"name":"release","path":".github/workflows/release.yml","event":"push","status":"completed","conclusion":"success","created_at":"2026-10-19T10:00:00Z","head_sha":"'$SHA'"}]}'
    ;;
  pr)
    # run de PULL_REQUEST com o mesmo SHA — não conta (o gate é sobre o CI
    # do push no main; um run de PR não valida a tag).
    emit '{"workflow_runs":[{"id":7,"name":"ci","path":".github/workflows/ci.yml","event":"pull_request","status":"completed","conclusion":"success","created_at":"2026-10-19T10:00:00Z","head_sha":"'$SHA'"}]}'
    ;;
  api500)
    echo "gh: HTTP 500 (Internal Server Error)" >&2
    exit 1
    ;;
  api403)
    echo "gh: HTTP 403 (Forbidden)" >&2
    exit 1
    ;;
  *)
    echo "modo desconhecido: $FAKE_GH_MODE" >&2; exit 99
    ;;
esac
FAKE
chmod +x "${TMP}/gh"

run_case() {
  local mode="$1" expect="$2" label="$3" timeout="${4:-900}" poll="${5:-30}" expect_out="${6:-}"
  export FAKE_GH_MODE="$mode"
  export FAKE_GH_COUNTER="${TMP}/counter-${mode}"
  rm -f "$FAKE_GH_COUNTER"
  echo "──────────────────────────────────────────────────────────────"
  echo "CENÁRIO: ${label}  (modo=${mode}, espera=${expect}, timeout=${timeout}s, poll=${poll}s)"
  echo "──────────────────────────────────────────────────────────────"
  set +e
  out="$(PATH="${TMP}:$PATH" \
         GITHUB_REPOSITORY="ils15/curupira-gis" \
         CI_TIMEOUT_SECONDS="$timeout" \
         CI_POLL_SECONDS="$poll" \
         bash "$SCRIPT" "$SHA" ci.yml 2>&1)"
  rc=$?
  set -e
  echo "$out"
  echo "-> exit=${rc} (esperado=${expect})"
  local ok=1
  if [ "$rc" != "$expect" ]; then
    echo "-> RESULTADO: FALHOU ❌ (esperado ${expect}, obtido ${rc})"
    ok=0
  elif [ -n "$expect_out" ] && ! printf '%s' "$out" | grep -qF "$expect_out"; then
    echo "-> RESULTADO: FALHOU ❌ (saída não contém '${expect_out}')"
    ok=0
  elif [ "$ok" = "1" ]; then
    echo "-> RESULTADO: OK ✅"
  fi
  [ "$ok" = "0" ] && FAILED=1
  echo
}

FAILED=0
run_case success   0 "CI já verde (conclusion=success)"
run_case progress  0 "CI em andamento -> depois verde (in_progress → success)" 60 1
run_case failure   1 "CI vermelho (conclusion=failure)"
run_case missing   1 "nenhum run de CI para o SHA (timeout)" 3 1
run_case other     1 "só existe run de outro workflow (release) — não conta como CI" 3 1
run_case pr        1 "só existe run de pull_request com o mesmo SHA — não conta" 3 1
run_case api500    1 "API do GitHub quebrada (HTTP 500) — falha fechado" 3 1 "::error::falha na API do GitHub"
run_case api403    1 "API do GitHub sem permissão (HTTP 403) — falha fechado" 3 1 "::error::falha na API do GitHub"

echo "══════════════════════════════════════════════════════════════"
if [ "$FAILED" = "0" ]; then
  echo "TODOS OS CENÁRIOS PASSARAM ✅"
else
  echo "HUVE FALHA ❌"
fi
exit "$FAILED"
