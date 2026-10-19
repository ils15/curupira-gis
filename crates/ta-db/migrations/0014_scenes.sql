-- 0014_scenes.sql — cenas de sensoriamento e o exchange do fluxo (Fase 4).
--
-- `scenes` registra a ORIGEM de uma observação (ex.: passagem de satélite):
-- nome único no BANCO (cenas são globais — podem servir a vários projetos),
-- aquisição e footprint opcionais (MultiPolygon em EPSG:4674, armazenamento
-- canônico) e metadados livres. O vínculo com projetos é N:N
-- (`project_scenes`); apagar o projeto ou a cena remove o vínculo (CASCADE).
--
-- `cell_measures.scene_id` é a proveniência da observação: **nullable** (uma
-- medida pode existir sem cena — observação importada a dedo). Apagar a cena
-- desvincula a medida (SET NULL), NUNCA apaga a medida.
--
-- **Política de concorrência de medidas: last-write-wins** por
-- `(cell_layer_id, cell_id, measure)` — a PK permanece INALTERADA (a cena é
-- proveniência da observação, não chave da medida). `measure set` e o import
-- de exchange fazem `ON CONFLICT DO UPDATE`: a última escrita vence, com
-- `updated_on` do banco como registro do momento.
--
-- `projects.description` hospeda a descrição do projeto referenciada pela
-- seção `project` do exchange (`curupira-monitoring-exchange v1`); nenhum
-- comando a escreve hoje — o dado entra/sai pelo exchange.
--
-- NÃO editar 0001–0013: bancos de dev já as aplicaram e o runner do sqlx
-- rastreia por versão/checksum. Idempotente (`IF NOT EXISTS`/
-- `ADD COLUMN IF NOT EXISTS`).

CREATE TABLE IF NOT EXISTS scenes (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Nome único no BANCO (cena é global, não por projeto).
    name          TEXT NOT NULL UNIQUE,
    -- Momento da aquisição (opcional — observação sem data conhecida é válida).
    acquisition_on TIMESTAMPTZ,
    -- Footprint da cena em EPSG:4674 (opcional — cena sem footprint é válida).
    footprint     GEOMETRY(MultiPolygon, 4674),
    -- Metadados livres (ex.: satélite, órbita/ponto, nível de processamento).
    metadata      JSONB NOT NULL DEFAULT '{}',
    created_on    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_on    TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE scenes IS
    'Cena de sensoriamento: origem de observações; global, vinculada a projetos por project_scenes.';

-- Vínculo N:N cena ↔ projeto.
CREATE TABLE IF NOT EXISTS project_scenes (
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    scene_id   UUID NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    linked_on  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, scene_id)
);

-- Consulta de cenas de um projeto (GUI/CLI) sem varrer todas as cenas.
CREATE INDEX IF NOT EXISTS project_scenes_scene_idx ON project_scenes (scene_id);

-- Proveniência da medida: apagar a cena desvincula (SET NULL), não apaga a
-- medida; a PK (cell_layer_id, cell_id, measure) e o upsert last-write-wins
-- ficam como estão.
ALTER TABLE cell_measures
    ADD COLUMN IF NOT EXISTS scene_id UUID REFERENCES scenes(id) ON DELETE SET NULL;

-- Resolução de proveniência por cena (medidas de uma cena, GUI/exchange).
CREATE INDEX IF NOT EXISTS cell_measures_scene_idx ON cell_measures (scene_id);

-- Descrição do projeto — seção `project` do exchange (ver cabeçalho).
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS description TEXT NOT NULL DEFAULT '';
