-- 0010_rules.sql — regras de classificação e rulesets versionados.
--
-- `rulesets` é uma linhagem `(project_id, name)` com versões crescentes: cada
-- escrita do mesmo nome grava a próxima `version` (1, 2, …), preservando o
-- histórico. `UNIQUE(project_id, name, version)` garante que a versão não se
-- repete; o repositório calcula `MAX(version)+1` e reexecuta uma vez em caso
-- de corrida entre duas escritas simultâneas.
--
-- `rules` associa um predicado textual a uma classe. `priority` maior vence
-- (o desempate por nome é do motor puro); `is_active = false` desliga a regra
-- sem apagá-la. O predicado é validado pela CLI (`ta_engine::parse_predicate`)
-- antes do insert — o banco garante apenas que não é vazio.
--
-- Tudo idempotente (`IF NOT EXISTS`); nomes próprios (Decisão 2 do plano).

CREATE TABLE IF NOT EXISTS rulesets (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    version     INTEGER NOT NULL CHECK (version > 0),
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name, version)
);

CREATE INDEX IF NOT EXISTS rulesets_project_name_idx
    ON rulesets (project_id, name);

CREATE TABLE IF NOT EXISTS rules (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ruleset_id UUID NOT NULL REFERENCES rulesets(id) ON DELETE CASCADE,
    class_id   UUID NOT NULL REFERENCES classes(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    -- Maior prioridade vence; empate desempata pelo menor nome (motor puro).
    priority   INTEGER NOT NULL DEFAULT 0,
    predicate  TEXT NOT NULL CHECK (length(btrim(predicate)) > 0),
    is_active  BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (ruleset_id, name)
);

CREATE INDEX IF NOT EXISTS rules_ruleset_priority_idx
    ON rules (ruleset_id, priority DESC);
