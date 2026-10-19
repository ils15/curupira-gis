#!/usr/bin/env bash
# Cria a tag `v<versão do manifesto>` quando um Release PR do release-please
# acabou de ser mergeado e a tag ainda não existe.
#
# ── POR QUE ESTE SCRIPT EXISTE (e não o release-please) ────────────────────
# O release-please-config.json usa `skip-github-release: true` porque a
# GitHub Release (draft, com os assets) é criada pelo release.yml. No
# upstream, a tag é efeito colateral da criação da Release: em
# googleapis/release-please, `src/strategies/base.ts` faz
#
#     if (this.skipGitHubRelease) { return; }   // buildRelease()
#
# e `buildReleases()` devolve [] -> não há candidato de release -> NENHUMA tag
# é criada (confirmado pelo upstream: test/manifest.ts, caso
# "should build releases" — o pacote com skipGithubRelease some do resultado).
# A própria doc do upstream avisa: "this option should only be used if you
# have existing infrastructure to tag these releases".
#
# Este script É essa infraestrutura. E, por derivar a tag do manifesto
# (fonte única da versão, atualizada pelo Release PR), a tag NÃO PODE
# divergir do [workspace.package] version do Cargo.toml — que é exatamente a
# divergência que o release-please elimina.
#
# ── CONTRATO ───────────────────────────────────────────────────────────────
#   entrada : .release-please-manifest.json (chave "." = versão), lida no
#             commit GITHUB_SHA (independente do estado do working tree)
#   saída   : tag="v<versão>" no $GITHUB_OUTPUT (vazio = nada a fazer)
#   env     : GITHUB_REPOSITORY, GITHUB_OUTPUT, GH_TOKEN, GITHUB_SHA,
#             GITHUB_EVENT_BEFORE (SHA anterior do push)
#
# Regras:
#   * a tag só é criada se o manifesto tiver sido bumpado desde o início do
#     push (ou desde o commit anterior, no dispatch manual) — evita taguear
#     em pushes comuns;
#   * no bootstrap (manifesto recém-criado, sem versão anterior) não tagueia;
#   * se a tag já existir, não faz nada (idempotente).
set -euo pipefail

REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY é obrigatório}"
MANIFEST="${MANIFEST_FILE:-.release-please-manifest.json}"
OUTPUT="${GITHUB_OUTPUT:-/dev/null}"

log_notice() { echo "::notice::$*"; }

# SHA do commit que está sendo processado (o merge do Release PR). Não usar
# `git rev-parse HEAD`: a action do release-please mexe no working tree
# (cria um branch local) — a tag SEMPRE aponta no commit do push.
sha="${GITHUB_SHA:-$(git rev-parse HEAD)}"

# Conteúdo do manifesto no commit alvo (independente do working tree).
if git cat-file -e "${sha}:${MANIFEST}" 2>/dev/null; then
  manifest_content="$(git show "${sha}:${MANIFEST}")"
elif [ -f "$MANIFEST" ]; then
  manifest_content="$(cat "$MANIFEST")"
else
  log_notice "manifesto ($MANIFEST) ausente em ${sha} — nada a taguear"
  exit 0
fi

version="$(jq -r '.["."] // empty' <<<"${manifest_content}" 2>/dev/null || true)"
if [ -z "$version" ]; then
  log_notice "manifesto sem a entrada '.' — nada a taguear"
  exit 0
fi

tag="v${version}"

# A tag já existe? (idempotência — e protege contra re-run do workflow)
if gh api "repos/${REPO}/git/ref/tags/${tag}" >/dev/null 2>&1; then
  log_notice "tag ${tag} já existe — nada a fazer"
  exit 0
fi

# O manifesto acabou de ser bumpado neste push?
#
# `GITHUB_EVENT_BEFORE` é o SHA anterior do push (o "before" do evento
# push) — usar o limite do push (e não HEAD^) cobre o caso de um push com
# mais de um commit (merge do Release PR + outro commit). Sem ele
# (workflow_dispatch), cai para HEAD^.
before_ref="HEAD^"
if [ -n "${GITHUB_EVENT_BEFORE:-}" ] &&
   [ "${GITHUB_EVENT_BEFORE}" != "0000000000000000000000000000000000000000" ]; then
  before_ref="${GITHUB_EVENT_BEFORE}"
fi

prev_manifest="$(git show "${before_ref}:${MANIFEST}" 2>/dev/null || true)"
if [ -z "$prev_manifest" ]; then
  log_notice "manifesto ainda não existia em ${before_ref} (bootstrap) — não tagueia"
  exit 0
fi
prev_version="$(jq -r '.["."] // empty' <<<"${prev_manifest}" 2>/dev/null || true)"
if [ "$prev_version" = "$version" ]; then
  log_notice "versão do manifesto não mudou desde ${before_ref} (${version}) — nada a taguear"
  exit 0
fi
log_notice "manifesto bumpado: ${prev_version} -> ${version}"

# Cria a ref da tag no commit do push (o merge do Release PR).
#
# HTTP 422 "Reference already exists" = a tag foi criada ENTRE o GET acima e
# este POST (dois runs concorrentes do release-please, ou um re-run do
# workflow). O resultado pretendido — a tag existe — já foi alcançado, então
# isso é IDEMPOTÊNCIA REAL: o script avisa e sai com 0 (sem abortar o job
# com um erro pouco claro). Qualquer outro erro da API FALHA o job.
if ! create_out="$(gh api --method POST "repos/${REPO}/git/refs" \
  -f ref="refs/tags/${tag}" \
  -f sha="${sha}" 2>&1)"; then
  if printf '%s' "$create_out" | grep -qi "422" &&
     printf '%s' "$create_out" | grep -qi "already exists"; then
    log_notice "tag ${tag} já existe (HTTP 422 na criação da ref) — nada a fazer"
    exit 0
  fi
  echo "::error::falha ao criar a tag ${tag} no commit ${sha}: ${create_out}"
  exit 1
fi

echo "tag=${tag}" >>"${OUTPUT}"
log_notice "tag ${tag} criada em ${sha} — disparando o build da release"
