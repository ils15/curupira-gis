#!/usr/bin/env bash
# Teste LOCAL do scripts/create-release-tag.sh com um `gh` falso (API do
# GitHub) e um repositório git temporário. Roda no CI (job `release-scripts`
# do ci.yml) — é a evidência de que a tag só é criada quando o manifesto foi
# bumpado (e de que a criação é idempotente).
#
# Uso: bash scripts/test-create-release-tag.sh
set -uo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_DIR}/scripts/create-release-tag.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

FAILED=0

# ── `gh` falso ─────────────────────────────────────────────────────────────
cat >"${TMP}/gh" <<'FAKE'
#!/usr/bin/env bash
# GET  repos/OWNER/REPO/git/ref/tags/<tag>  -> 404 se a tag não existir
# POST repos/OWNER/REPO/git/refs             -> cria a ref
#                                       ... ou 422 se FAKE_GH_POST_422=1
url=""
for a in "$@"; do
  case "$a" in
    repos/*) url="$a" ;;
  esac
done
case "$url" in
  *"/git/ref/tags/"*)
    tag="${url##*/git/ref/tags/}"
    if [ -f "${FAKE_GH_TAGS}/${tag}" ]; then
      printf '{"ref":"refs/tags/%s"}' "$tag"
    else
      echo "gh: Not Found (404)" >&2
      exit 1
    fi
    ;;
  *"/git/refs")
    ref=""; sha=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -f) shift; case "$1" in ref=*) ref="${1#ref=}";; sha=*) sha="${1#sha=}";; esac ;;
      esac
      shift
    done
    tag="${ref##*/}"
    # Corrida: a tag foi criada entre o GET e o POST (a API responde 422
    # "Reference already exists"). O script tem que tratar como "já
    # existe" — sem abortar o job.
    if [ -n "${FAKE_GH_POST_422:-}" ]; then
      echo "gh: HTTP 422: Validation Failed (Reference already exists)" >&2
      echo '{"message":"Reference already exists"}' >&2
      exit 1
    fi
    echo "$sha" >"${FAKE_GH_TAGS}/${tag}"
    printf '{"ref":"%s","object":{"sha":"%s"}}' "$ref" "$sha"
    ;;
  *)
    echo "gh fake: url não suportada: '$url' (args: $*)" >&2
    exit 2
    ;;
esac
FAKE
chmod +x "${TMP}/gh"

make_repo() {
  local dir="$1"
  mkdir -p "$dir"
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t
  git -C "$dir" config user.name t
}

commit_manifest() {
  local dir="$1" version="$2" extra="${3:-}"
  printf '{\n  ".": "%s"\n}\n' "$version" >"${dir}/.release-please-manifest.json"
  [ -n "$extra" ] && echo "$extra" >"${dir}/extra.txt"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m "chore(release): ${version}" || true
}

# ── cenários (funções — sem `eval`: nada é re-interpretado pelo shell) ─────
# As funções usam `work`/`tags` (locais de run_case, escopo dinâmico do bash).

sc_bootstrap() {
  commit_manifest "$work" 0.3.0
}

sc_no_bump() {
  commit_manifest "$work" 0.3.0
  commit_manifest "$work" 0.3.0 "arquivo qualquer"
}

sc_bump() {
  commit_manifest "$work" 0.3.0
  commit_manifest "$work" 0.4.0-alpha
}

sc_tag_exists() {
  sc_bump
  echo x >"${tags}/v0.4.0-alpha"
}

sc_race_422() {
  # A tag foi criada entre o GET e o POST (a API responde 422).
  sc_bump
  export FAKE_GH_POST_422=1
}

run_case() {
  local label="$1" expect_created="$2" event="$3" setup_fn="$4" expect_out="${5:-}" expect_tag="${6:-any}"
  echo "──────────────────────────────────────────────────────────────"
  echo "CENÁRIO: ${label}  (event=${event})"
  echo "──────────────────────────────────────────────────────────────"
  local work="${TMP}/work-$RANDOM$RANDOM"
  local tags="${TMP}/tags-$RANDOM$RANDOM"
  mkdir -p "$tags"
  make_repo "$work"
  unset FAKE_GH_POST_422
  "$setup_fn"

  local before after
  before="$(ls -1 "$tags" | wc -l | tr -d ' ')"

  local out rc
  out="$(cd "$work" && PATH="${TMP}:$PATH" \
        GITHUB_REPOSITORY="ils15/curupira-gis" \
        GITHUB_EVENT_NAME="$event" \
        GITHUB_SHA="$(git -C "$work" rev-parse HEAD)" \
        GITHUB_OUTPUT="${work}/out.txt" \
        FAKE_GH_TAGS="$tags" \
        bash "$SCRIPT" 2>&1)"
  rc=$?
  echo "$out"
  after="$(ls -1 "$tags" | wc -l | tr -d ' ')"
  local created=$((after - before))
  local out_tag=""
  [ -f "${work}/out.txt" ] && out_tag="$(sed -n 's/^tag=//p' "${work}/out.txt")"
  echo "-> tags criadas=${created} (esperado ${expect_created}) | GITHUB_OUTPUT tag='${out_tag}' | exit=${rc}"
  local ok=1
  if [ "$created" != "$expect_created" ] || [ "$rc" != "0" ]; then
    echo "-> RESULTADO: FALHOU ❌"
    ok=0
  elif [ -n "$expect_out" ] && ! printf '%s' "$out" | grep -qF "$expect_out"; then
    echo "-> RESULTADO: FALHOU ❌ (saída não contém '${expect_out}')"
    ok=0
  elif [ "$expect_tag" != "any" ] && [ "$out_tag" != "$expect_tag" ]; then
    echo "-> RESULTADO: FALHOU ❌ (GITHUB_OUTPUT tag='${out_tag}', esperado '${expect_tag}')"
    ok=0
  elif [ "$ok" = "1" ]; then
    echo "-> RESULTADO: OK ✅"
  fi
  [ "$ok" = "0" ] && FAILED=1
  echo
}

# 1) bootstrap: o manifesto acabou de ser criado (não existe no commit anterior)
run_case "bootstrap — manifesto criado agora (não tagueia)" 0 push sc_bootstrap

# 2) push comum: o manifesto não mudou (0.3.0 -> 0.3.0)
run_case "push sem bump do manifesto (não tagueia)" 0 push sc_no_bump

# 3) Release PR mergeado: o manifesto mudou (0.3.0 -> 0.4.0-alpha)
run_case "Release PR mergeado — manifesto bumpado (tagueia v0.4.0-alpha)" 1 push sc_bump

# 4) a tag já existe (idempotência)
run_case "tag já existe (não tagueia de novo)" 0 push sc_tag_exists

# 5) workflow_dispatch (re-run manual) com o manifesto bumpado no commit
#    anterior -> tagueia
run_case "workflow_dispatch após bump do manifesto (tagueia)" 1 workflow_dispatch sc_bump

# 6) workflow_dispatch sem bump (bootstrap) -> não tagueia
run_case "workflow_dispatch sem bump (não tagueia)" 0 workflow_dispatch sc_bootstrap

# 7) corrida: a tag foi criada ENTRE o GET e o POST (HTTP 422) -> o script
#    trata como "já existe": sai com 0 e NÃO anuncia a tag no output
#    (o outro run, que criou a tag, é quem dispara o build).
run_case "corrida — POST responde 422 (tag já existe) — idempotência real" 0 push \
  sc_race_422 "já existe" ""

echo "══════════════════════════════════════════════════════════════"
if [ "$FAILED" = "0" ]; then
  echo "TODOS OS CENÁRIOS PASSARAM ✅"
else
  echo "HUVE FALHA ❌"
fi
exit "$FAILED"
