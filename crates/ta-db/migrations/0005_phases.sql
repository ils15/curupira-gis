-- 0005_phases.sql — fases de trabalho + parâmetros de cálculo de área.
--
-- `phases`: etapa do fluxo de monitoramento dentro de um projeto (agrupa AOIs
-- e grades de células; `cell_blocker` habilita o bloqueio de células).
-- `area_settings`: como a ÁREA do projeto é calculada — SRID métrico
-- (Decisão 4: EPSG:5880 Policônica SIRGAS2000 por padrão) e unidade de saída.
-- Geometria continua sendo persistida em 4674; só o cálculo de área usa 5880.
--
-- `projects` ganha os limites de escala e o teto de células da grade.
-- Tudo é idempotente (`ADD COLUMN IF NOT EXISTS`/`CREATE TABLE IF NOT EXISTS`)
-- e usa defaults sensatos, então re-rodar não quebra nem reescreve os dados.
-- Nomes de tabela/coluna são próprios do clean-room (Decisão 2).

ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS active boolean NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS min_scale double precision,
    ADD COLUMN IF NOT EXISTS max_scale double precision,
    -- Teto de células de uma grade do projeto. O motor puro (ta-engine) tem
    -- um teto próprio de memória; este é a política de negócio por projeto.
    ADD COLUMN IF NOT EXISTS max_cells integer NOT NULL DEFAULT 1000000,
    ADD COLUMN IF NOT EXISTS updated_on timestamptz NOT NULL DEFAULT now();

CREATE TABLE IF NOT EXISTS phases (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id   UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    cell_blocker BOOLEAN NOT NULL DEFAULT false,
    sort_order   INTEGER NOT NULL DEFAULT 0,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE IF NOT EXISTS area_settings (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- SRID métrico do cálculo de área (5880 = Policônica SIRGAS2000).
    srs_id     INTEGER NOT NULL DEFAULT 5880,
    -- Unidade de saída da área (ex.: ha, m2, km2).
    unit       TEXT NOT NULL DEFAULT 'ha',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Uma configuração de área por projeto.
    UNIQUE (project_id)
);

-- app_meta: metadados de aplicação/schema (ex.: versão do schema). Chave-valor
-- genérico e independente de projeto; `key` é única. Nenhum consumidor da CLI
-- ainda — a tabela é prevista pelo plano (Fase 1) e fica pronta para o versionamento.
CREATE TABLE IF NOT EXISTS app_meta (
    id         SERIAL PRIMARY KEY,
    key        TEXT NOT NULL UNIQUE,
    value      TEXT,
    updated_on TIMESTAMPTZ NOT NULL DEFAULT now()
);
