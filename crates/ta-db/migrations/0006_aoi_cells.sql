-- 0006_aoi_cells.sql — AOI, camadas de células, células e células bloqueadas.
--
-- A AOI NÃO é uma geometria nova: é uma camada vetorial existente
-- (`layers`) eleita como área de interesse. `aoi_layers` registra esse vínculo
-- (e a coluna de atributo que identifica cada feição da AOI).
--
-- `cell_layers` é a grade regular derivada de uma AOI: resolução (em GRAUS,
-- pois 4674 é geográfico) e origem do alinhamento. Cada `cells` é um polígono
-- 4674 com id determinístico (`"<col>_<row>"`) e os índices linha/coluna.
-- `blocked_cells` marca células fora do processamento (motivo livre).
--
-- `aoi_layers`/`cell_layers` podem pertencer a uma **fase** (`phases.id`), que
-- agrupa AOIs e grades de um passo do fluxo. O vínculo é opcional (nullable) e
-- cada camada pertence a no máximo UMA fase — um N:N exigiria tabela de junção,
-- que não é necessário hoje (YAGNI; revisa-se se surgir a demanda).
--
-- Tudo é idempotente (`IF NOT EXISTS`/`ADD COLUMN IF NOT EXISTS`); nomes
-- próprios do clean-room (Decisão 2). Índice GIST em `cells.geom` para
-- consultas espaciais.

CREATE TABLE IF NOT EXISTS aoi_layers (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id    UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    layer_id      UUID NOT NULL REFERENCES layers(id) ON DELETE CASCADE,
    -- Coluna de atributo que identifica a AOI (opcional).
    aoi_id_column TEXT,
    -- Fase dona da AOI (opcional); apagar a fase desvincula, não apaga a AOI.
    phase_id      UUID REFERENCES phases(id) ON DELETE SET NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A mesma camada não é registrada duas vezes como AOI do projeto.
    UNIQUE (project_id, layer_id)
);

-- Bancos criados antes deste ajuste não têm a coluna (o CREATE acima é ignorado
-- quando a tabela já existe) — adiciona de forma idempotente.
ALTER TABLE aoi_layers
    ADD COLUMN IF NOT EXISTS phase_id UUID REFERENCES phases(id) ON DELETE SET NULL;

-- Índice para filtrar AOIs por fase (convenção de nome: <table>_<col>_idx).
CREATE INDEX IF NOT EXISTS aoi_layers_phase_id_idx ON aoi_layers (phase_id);

CREATE TABLE IF NOT EXISTS cell_layers (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id   UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    aoi_layer_id UUID NOT NULL REFERENCES aoi_layers(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    -- Passo da grade EM GRAUS (4674 é geográfico) — deve ser > 0.
    resolution   DOUBLE PRECISION NOT NULL CHECK (resolution > 0),
    origin_x     DOUBLE PRECISION NOT NULL,
    origin_y     DOUBLE PRECISION NOT NULL,
    -- Fase dona da grade (opcional); apagar a fase desvincula, não apaga a grade.
    phase_id     UUID REFERENCES phases(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

ALTER TABLE cell_layers
    ADD COLUMN IF NOT EXISTS phase_id UUID REFERENCES phases(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS cell_layers_phase_id_idx ON cell_layers (phase_id);

-- `cell_layers.srid` era gravado e nunca lido (o repositório sempre persiste em
-- 4674 via `spatial::TARGET_SRID`): a coluna era enganosa e é removida. O SRID
-- das células passa a ser **validado na criação** da camada (repo_cell), sem
-- armazenamento redundante.
ALTER TABLE cell_layers DROP COLUMN IF EXISTS srid;

CREATE TABLE IF NOT EXISTS cells (
    id            BIGSERIAL PRIMARY KEY,
    cell_layer_id UUID NOT NULL REFERENCES cell_layers(id) ON DELETE CASCADE,
    -- Identificador determinístico do motor de grade: "<col>_<row>" (0-based).
    cell_id       TEXT NOT NULL,
    geom          GEOMETRY(Polygon, 4674) NOT NULL,
    row_index     INTEGER NOT NULL,
    col_index     INTEGER NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Coberto por índice e alvo da FK composta de `blocked_cells`.
    UNIQUE (cell_layer_id, cell_id)
);

-- Índice espacial de `cells.geom`. O nome antigo (`cells_geom_gist`) foi
-- alinhado à convenção do repo (`<table>_<col>_idx`).
DROP INDEX IF EXISTS cells_geom_gist;
CREATE INDEX IF NOT EXISTS cells_geom_idx ON cells USING GIST (geom);

CREATE TABLE IF NOT EXISTS blocked_cells (
    id            BIGSERIAL PRIMARY KEY,
    cell_layer_id UUID NOT NULL REFERENCES cell_layers(id) ON DELETE CASCADE,
    cell_id       TEXT NOT NULL,
    reason        TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Uma célula é bloqueada no máximo uma vez por camada.
    UNIQUE (cell_layer_id, cell_id)
);

-- `blocked_cells` só existe para células que existem: FK composta para
-- `cells(cell_layer_id, cell_id)` (coberta pelo UNIQUE de `cells`). Apagar a
-- célula remove o bloqueio (CASCADE). Guarda em `pg_constraint` mantém o DDL
-- idempotente (mesmo padrão do 0003_indexes).
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'blocked_cells_cell_fk'
          AND conrelid = 'blocked_cells'::regclass
          AND contype = 'f'
    ) THEN
        ALTER TABLE blocked_cells
            ADD CONSTRAINT blocked_cells_cell_fk
            FOREIGN KEY (cell_layer_id, cell_id)
            REFERENCES cells (cell_layer_id, cell_id)
            ON DELETE CASCADE;
    END IF;
END
$$;
