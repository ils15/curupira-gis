-- 0001_init.sql — schema base do terraamazon-cleanroom-rust.
-- Banco-alvo: PostgreSQL 16 (gen_random_uuid() é nativo desde o PG13).
-- SRID canônico do projeto: EPSG:4674 (SIRGAS 2000), enforce por CHECK aqui
-- e por typmod geometry(...,4674) na coluna espacial da camada.

CREATE TABLE projects (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name       TEXT NOT NULL UNIQUE,
    srid       INT  NOT NULL DEFAULT 4674,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE layers (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    geom_type  TEXT NOT NULL CHECK (geom_type IN (
        'point', 'linestring', 'polygon',
        'multipoint', 'multilinestring', 'multipolygon',
        'geometry')),
    -- Camadas vetoriais nascem e permanecem em 4674; reprojeção acontece na
    -- ingestão (ta-gis::normalizer), nunca na persistência.
    srid       INT  NOT NULL DEFAULT 4674 CHECK (srid = 4674),
    -- Bounding box materializado da camada (atualizado por LayerRepo::update_bbox).
    bbox       GEOMETRY(Geometry, 4674),
    source_uri TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);
