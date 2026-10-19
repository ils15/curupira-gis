#!/usr/bin/env bash
# Aguarda o run de CI (workflow .github/workflows/ci.yml) do MESMO SHA da tag
# ficar verde. Sai != 0 se o CI ficar vermelho, for cancelado, estourar o
# tempo limite, se nenhum run de CI existir para o SHA, ou se a API do GitHub
# falhar (500/403/etc — falha fechada).
#
# ── USO ────────────────────────────────────────────────────────────────────
#   scripts/wait-for-ci.sh <sha> [workflow-arquivo]
#
# ── ENV ────────────────────────────────────────────────────────────────────
#   GITHUB_REPOSITORY    owner/repo (obrigatório)
#   CI_TIMEOUT_SECONDS   limite de espera (padrão 900 = 15 min)
#   CI_POLL_SECONDS      intervalo entre polls (padrão 30)
#
# ── CONTRATO ───────────────────────────────────────────────────────────────
#   saída 0 = CI verde no SHA dado
#   saída 1 = CI vermelho/cancelado/sem run/timeout/erro de API
set -euo pipefail

REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY é obrigatório}"
SHA="${1:?uso: wait-for-ci.sh <sha> [workflow-arquivo]}"
WORKFLOW="${2:-ci.yml}"
TIMEOUT="${CI_TIMEOUT_SECONDS:-900}"
INTERVAL="${CI_POLL_SECONDS:-30}"

deadline=$(( $(date +%s) + TIMEOUT ))

# Estado do run de CI mais recente para o SHA: "<status> <conclusion>".
# status: queued | in_progress | completed | none | api_error
# conclusion: success | failure | cancelled | timed_out | ... | none
run_status=""
run_conclusion=""

run_state() {
  local raw state

  # `--paginate`: o endpoint de runs é paginado (100 por página) — com
  # vários re-runs do mesmo SHA, a página mais recente poderia ficar de
  # fora. O `jq -s` (slurp) junta as páginas em um único array.
  #
  # `event == "push"`: só conta o run disparado por PUSH. Um run de
  # `pull_request` com o mesmo SHA (mesmo commit testado no PR) não
  # valida a tag — o gate é sobre o CI do `main`.
  #
  # Falha da API (500/403/timeout) ou JSON inválido => "api_error": o
  # script falha FECHADO (nunca "CI verde" por acidente).
  if ! raw="$(gh api --paginate "repos/${REPO}/actions/runs?head_sha=${SHA}&per_page=100" 2>&1)"; then
    run_status="api_error"
    run_conclusion="$(printf '%s' "$raw" | tr '\n' ' ' | cut -c1-500)"
    return 0
  fi

  if ! state="$(printf '%s' "$raw" | jq -s -r --arg wf "${WORKFLOW}" '
      [ .[]
        | .workflow_runs[]?
        | select((.path // "") | endswith($wf))
        | select((.event // "") == "push")
      ]
      | sort_by(.created_at)
      | last
      | if . == null
        then "none none"
        else "\(.status) \(.conclusion // "none")"
        end
    ' 2>&1)"; then
    run_status="api_error"
    run_conclusion="resposta inválida da API: $(printf '%s' "$state" | tr '\n' ' ' | cut -c1-500)"
    return 0
  fi

  run_status="${state%% *}"
  run_conclusion="${state##* }"
}

while :; do
  run_state
  status="${run_status}"
  conclusion="${run_conclusion}"

  case "${status}" in
    api_error)
      # Falha fechada: erro de API (500/403) NÃO pode virar "CI verde" nem
      # "nenhum run" — o job tem que falhar, com anotação, no lugar certo.
      echo "::error::falha na API do GitHub ao consultar os runs de ${WORKFLOW} para ${SHA}: ${conclusion}"
      exit 1
      ;;
    completed)
      if [ "${conclusion}" = "success" ]; then
        echo "CI verde para ${SHA} (workflow ${WORKFLOW})"
        exit 0
      fi
      echo "::error::CI não passou para ${SHA}: ${conclusion}"
      exit 1
      ;;
    none)
      # O run pode ainda não existir (o CI do mesmo push pode estar
      # enfileirado). Continua polling até o limite; só falha no timeout.
      if [ "$(date +%s)" -ge "${deadline}" ]; then
        echo "::error::CI não passou para ${SHA}: nenhum run de ${WORKFLOW} encontrado em ${TIMEOUT}s"
        exit 1
      fi
      echo "nenhum run de ${WORKFLOW} para ${SHA} ainda — aguardando ${INTERVAL}s..."
      sleep "${INTERVAL}"
      ;;
    *)
      # queued | in_progress | waiting | requested | pending
      if [ "$(date +%s)" -ge "${deadline}" ]; then
        echo "::error::CI não passou para ${SHA}: status=${status} conclusion=${conclusion} após ${TIMEOUT}s"
        exit 1
      fi
      echo "CI ainda em andamento para ${SHA} (status=${status}) — aguardando ${INTERVAL}s..."
      sleep "${INTERVAL}"
      ;;
  esac
done
