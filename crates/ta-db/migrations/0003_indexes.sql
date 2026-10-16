-- 0003_indexes.sql — índice de lookup por camada + CHECK canônico de SRID.
--
-- - Índice b-tree em features.layer_id: a FK não gera índice automático no
--   PostgreSQL; sem ele, filtros por camada (FeatureRepo por layer) e o
--   ON DELETE CASCADE de layers fazem seq scan na tabela de feições.
--   IF NOT EXISTS mantém o DDL idempotente para re-runs manuais.
-- - projects.srid passa a enforce 4674 por CHECK (mesma política já vigente
--   em layers.srid e features_srid_4674), via DO block idempotente: consulta
--   pg_constraint antes de adicionar, então re-executar não falha.

CREATE INDEX IF NOT EXISTS features_layer_id_idx ON features (layer_id);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'projects_srid_check'
          AND conrelid = 'projects'::regclass
          AND contype = 'c'
    ) THEN
        ALTER TABLE projects
            ADD CONSTRAINT projects_srid_check CHECK (srid = 4674);
    END IF;
END
$$;
