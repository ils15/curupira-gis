#!/usr/bin/env python3
"""Regressão do B-1 — todo checkout de BUILD do release.yml usa `ref:` da tag.

O B-1 (gate do Themis, 2026-10-19): no caminho `workflow_call` (release-please
-> release.yml) os 5 `actions/checkout` estavam sem `with: ref:` — o default do
checkout é `github.ref` do RUN, que no `workflow_call` é `refs/heads/main` (o
ref do caller). Resultado: o gate `wait-for-ci` validava o CI **da tag**
enquanto os assets saíam de **main** (e um `workflow_dispatch` de tag antiga
publicaria binário de `main` com o nome da tag antiga, sem erro).

Este teste é o guard de regressão: ele roda NO CI (job `release-scripts` do
ci.yml) e falha se:
  * algum job de build deixar de declarar `with.ref` no checkout;
  * o `ref:` deixar de cobrir os 3 gatilhos (tem que mencionar `inputs.tag`
    — o caminho `workflow_call`/`workflow_dispatch` — e `github.ref` — o
    caminho `push: tags`);
  * a guarda de SHA (comparar o SHA do workspace com o da tag) sumir de
    algum job de build;
  * o job `wait-for-ci` deixar de exportar o SHA resolvido;
  * a concorrência do ci.yml voltar a ser por `ref` no `main` (M-2).

Dois parsers são usados e comparados: PyYAML (quando disponível) e um
parser de stdlib (fallback, para o teste não depender de pip). Se os dois
discordarem, o teste falha — assim o fallback não apodrece.

Uso: python3 scripts/test-workflow-refs.py
Exit: 0 = tudo certo; 1 = regressão.
"""

from __future__ import annotations

import os
import re
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RELEASE_YML = os.path.join(REPO_ROOT, ".github", "workflows", "release.yml")
CI_YML = os.path.join(REPO_ROOT, ".github", "workflows", "ci.yml")

# Jobs que COMPILAM/EMBALAM (o código tem que ser o da tag).
BUILD_JOBS = (
    "verify-version",
    "build-linux",
    "build-windows",
    "build-macos",
    "sbom",
)

# A expressão de `ref:` precisa cobrir os 3 gatilhos do release.yml:
#   push: tags        -> github.ref (já é a própria tag)
#   workflow_call     -> inputs.tag
#   workflow_dispatch -> inputs.tag
REF_REQUIRED = ("inputs.tag", "github.ref")

# Marcador da guarda de SHA (a mensagem de erro é o contrato do B-1).
GUARD_MARKER = "build a partir de"

# Output do job `wait-for-ci` com o SHA resolvido da tag.
SHA_OUTPUT = "needs.wait-for-ci.outputs.sha"

# Tokens extras por job: o `verify-version` NÃO pode depender do
# `wait-for-ci` (ciclo) — ele resolve o SHA da tag pela API.
JOB_EXTRA_TOKENS = {
    "verify-version": ("/commits/",),
    "build-linux": (SHA_OUTPUT,),
    "build-windows": (SHA_OUTPUT,),
    "build-macos": (SHA_OUTPUT,),
    "sbom": (SHA_OUTPUT,),
}


# ── parser stdlib (fallback, sem PyYAML) ────────────────────────────────────
def _indent(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _parse_step_body(lines, j, step):
    """Lê o corpo de um step (chaves em indent 8) e devolve o próximo índice."""
    n = len(lines)
    while j < n:
        line = lines[j]
        if not line.strip():
            j += 1
            continue
        if _indent(line) <= 6:
            break
        m = re.match(r"^        ([A-Za-z0-9_.-]+):\s*(.*)$", line)
        if not m:
            j += 1
            continue
        key, val = m.group(1), m.group(2)
        if key in ("with", "env"):
            target = step[key]
            j += 1
            while j < n:
                line2 = lines[j]
                if not line2.strip():
                    j += 1
                    continue
                if _indent(line2) <= 8:
                    break
                m2 = re.match(r"^          ([A-Za-z0-9_.-]+):\s*(.*)$", line2)
                if m2:
                    target[m2.group(1)] = m2.group(2).strip()
                j += 1
            continue
        if key == "run":
            if val in ("|", ">", "|-", ">-", "|+", ">+"):
                body = []
                j += 1
                while j < n:
                    line2 = lines[j]
                    if not line2.strip():
                        body.append("")
                        j += 1
                        continue
                    if _indent(line2) <= 8:
                        break
                    body.append(line2.strip())
                    j += 1
                step["run"] = "\n".join(body)
                continue
            step["run"] = val
        elif key == "uses":
            step["uses"] = val
        elif key == "name":
            step["name"] = val
        j += 1
    return j


def parse_fallback(text: str):
    """Parser de stdlib: {jobs: {...}, top: {...}}."""
    lines = text.splitlines()
    jobs = {}
    top = {}
    current = None
    in_jobs = False
    i = 0
    n = len(lines)
    while i < n:
        line = lines[i]
        if not line.strip() or line.lstrip().startswith("#"):
            i += 1
            continue

        # chave de topo (indent 0)
        m = re.match(r"^([A-Za-z0-9_.-]+):\s*(.*)$", line)
        if m and _indent(line) == 0:
            key, val = m.group(1), m.group(2)
            in_jobs = key == "jobs"
            current = None
            if key == "jobs":
                # o bloco `jobs:` é percorrido job a job (indent 2) — não
                # pode ser consumido aqui como um bloco de topo comum.
                i += 1
                continue
            if val:
                top[key] = val
                i += 1
                continue
            # bloco de topo: coleta as chaves de indent 2
            j = i + 1
            while j < n:
                line2 = lines[j]
                if not line2.strip():
                    j += 1
                    continue
                if _indent(line2) <= 0:
                    break
                m2 = re.match(r"^  ([A-Za-z0-9_.-]+):\s*(.*)$", line2)
                if m2:
                    top[f"{key}.{m2.group(1)}"] = m2.group(2).strip()
                j += 1
            i = j
            continue

        # job (indent 2, dentro de `jobs:`)
        m = re.match(r"^  ([A-Za-z0-9_.-]+):\s*$", line)
        if m and in_jobs:
            current = m.group(1)
            jobs.setdefault(current, {"steps": [], "outputs": {}})
            i += 1
            continue

        # chave de job (indent 4)
        m = re.match(r"^    ([A-Za-z0-9_.-]+):\s*(.*)$", line)
        if m and current:
            key, val = m.group(1), m.group(2)
            if key == "outputs":
                j = i + 1
                while j < n:
                    line2 = lines[j]
                    if not line2.strip():
                        j += 1
                        continue
                    if _indent(line2) <= 4:
                        break
                    m2 = re.match(r"^      ([A-Za-z0-9_.-]+):\s*(.*)$", line2)
                    if m2:
                        jobs[current]["outputs"][m2.group(1)] = m2.group(2).strip()
                    j += 1
                i = j
                continue
            i += 1
            continue

        # step (indent 6)
        m = re.match(r"^      - (.*)$", line)
        if m and current:
            step = {"uses": None, "with": {}, "env": {}, "run": "", "name": ""}
            first = m.group(1)
            if first.startswith("uses:"):
                step["uses"] = first[len("uses:"):].strip()
            elif first.startswith("name:"):
                step["name"] = first[len("name:"):].strip()
            i = _parse_step_body(lines, i + 1, step)
            jobs[current]["steps"].append(step)
            continue

        i += 1
    return {"jobs": jobs, "top": top}


# ── parser PyYAML (primário) ───────────────────────────────────────────────
def parse_pyyaml(text: str):
    import yaml  # noqa: PLC0415 — import opcional (há fallback em stdlib)

    doc = yaml.safe_load(text) or {}
    jobs = {}
    for name, job in (doc.get("jobs") or {}).items():
        job = job or {}
        steps = []
        for s in job.get("steps") or []:
            s = s or {}
            steps.append(
                {
                    "uses": s.get("uses"),
                    "with": s.get("with") or {},
                    "env": s.get("env") or {},
                    "run": s.get("run") or "",
                    "name": s.get("name") or "",
                }
            )
        outputs = {}
        for k, v in (job.get("outputs") or {}).items():
            outputs[k] = "" if v is None else str(v)
        jobs[name] = {"steps": steps, "outputs": outputs}

    top = {}
    for k, v in doc.items():
        if isinstance(v, dict):
            for k2, v2 in v.items():
                top[f"{k}.{k2}"] = "" if v2 is None else str(v2)
        elif not isinstance(v, (list, bool)):
            top[k] = str(v)
    return {"jobs": jobs, "top": top}


# ── asserções ───────────────────────────────────────────────────────────────
def _blob(job) -> str:
    """Todo o texto "executável" do job (run + env + with)."""
    parts = []
    for s in job.get("steps") or []:
        parts.append(s.get("run") or "")
        parts.append(s.get("name") or "")
        parts.extend(str(v) for v in (s.get("env") or {}).values())
        parts.extend(str(v) for v in (s.get("with") or {}).values())
    return "\n".join(parts)


def check_release(parsed, label: str):
    errors = []
    jobs = parsed["jobs"]

    for job_name in BUILD_JOBS:
        job = jobs.get(job_name)
        if job is None:
            errors.append(f"job '{job_name}' não encontrado no release.yml")
            continue

        steps = job.get("steps") or []
        checkouts = [s for s in steps if (s.get("uses") or "").startswith("actions/checkout@")]
        if not checkouts:
            errors.append(f"job '{job_name}': nenhum checkout")
        for s in checkouts:
            ref = (s.get("with") or {}).get("ref")
            if not ref:
                errors.append(
                    f"job '{job_name}': checkout SEM 'with.ref' — no workflow_call "
                    "o default (github.ref do caller = main) compilaria main em vez da tag"
                )
                continue
            for token in REF_REQUIRED:
                if token not in ref:
                    errors.append(
                        f"job '{job_name}': ref='{ref}' não menciona '{token}' "
                        "(o ref tem que cobrir os 3 gatilhos)"
                    )

        blob = _blob(job)
        if GUARD_MARKER not in blob:
            errors.append(
                f"job '{job_name}': sem a guarda de SHA (esperado o erro "
                f"'{GUARD_MARKER} <sha1> mas a tag aponta para <sha2>')"
            )
        for token in JOB_EXTRA_TOKENS.get(job_name, ()):
            if token not in blob:
                errors.append(f"job '{job_name}': a guarda não usa '{token}'")

    wait = jobs.get("wait-for-ci")
    if wait is None:
        errors.append("job 'wait-for-ci' não encontrado no release.yml")
    else:
        sha_out = (wait.get("outputs") or {}).get("sha") or ""
        if "steps.resolve.outputs.sha" not in sha_out:
            errors.append(
                "job 'wait-for-ci': outputs.sha não referencia steps.resolve.outputs.sha "
                f"(valor: {sha_out!r})"
            )
    return errors


def check_ci(parsed, label: str):
    """M-2: a concorrência do ci.yml tem que ser por SHA no main (por ref no PR)."""
    errors = []
    group = (parsed.get("top") or {}).get("concurrency.group") or ""
    if "github.sha" not in group:
        errors.append(
            "ci.yml: concurrency.group não é por SHA no main — um push novo no main "
            f"cancelaria o CI da tag (valor: {group!r})"
        )
    return errors


def main() -> int:
    with open(RELEASE_YML, encoding="utf-8") as fh:
        release_text = fh.read()
    with open(CI_YML, encoding="utf-8") as fh:
        ci_text = fh.read()

    parsers = []
    try:
        import yaml  # noqa: F401 — só para saber se está disponível

        parsers.append(("pyyaml", parse_pyyaml))
    except ImportError:
        pass
    parsers.append(("stdlib", parse_fallback))

    all_errors = []
    signatures = {}
    for label, fn in parsers:
        try:
            release_parsed = fn(release_text)
            ci_parsed = fn(ci_text)
        except Exception as exc:  # noqa: BLE001 — erro de parse = teste vermelho
            all_errors.append(f"[{label}] falha ao parsear o YAML: {exc}")
            continue
        errs = check_release(release_parsed, label) + check_ci(ci_parsed, label)
        all_errors.extend(f"[{label}] {e}" for e in errs)
        signatures[label] = sorted(e.split("] ", 1)[-1] for e in errs)

    if len(signatures) > 1:
        vals = list(signatures.values())
        if any(v != vals[0] for v in vals[1:]):
            all_errors.append(
                f"os parsers discordam: {signatures} — o fallback em stdlib "
                "precisa acompanhar o PyYAML"
            )

    print("═" * 70)
    print("REGRESSÃO B-1: checkout de build do release.yml usa o ref: da tag")
    print("═" * 70)
    print(f"parsers usados: {', '.join(label for label, _ in parsers)}")
    for job_name in BUILD_JOBS:
        print(f"  - job '{job_name}': ref + guarda de SHA verificados")
    print("  - job 'wait-for-ci': output sha verificado")
    print("  - ci.yml: concorrência por SHA no main verificada")
    print()

    if all_errors:
        print("FALHOU ❌")
        for e in all_errors:
            print(f"  - {e}")
        return 1

    print("TODAS AS VERIFICAÇÕES PASSARAM ✅")
    return 0


if __name__ == "__main__":
    sys.exit(main())
