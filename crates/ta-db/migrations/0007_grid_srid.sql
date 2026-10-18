-- 0007_grid_srid.sql — `cell_layers.srid`: SRS em que a grade foi DEFINIDA.
--
-- Decisão 4 (retificada pelo usuário em 2026-10-18): a grade é configurável em
-- QUALQUER EPSG — `resolution`, `origin_x` e `origin_y` são expressos nas
-- unidades desse SRS (graus se geográfico, ex.: 4674; metros se projetado,
-- ex.: 5880). O armazenamento das células continua CANÔNICO em EPSG:4674
-- (`cells.geom geometry(Polygon,4674)`); a reprojeção acontece na fronteira de
-- IO com `ST_Transform`. Não há escolha excludente entre graus e metros.
--
-- A coluna diz em que SRS a grade foi definida (não é o SRID de persistência).
-- A FK para `spatial_ref_sys(srid)` (tabela de referência do EPSG criada pelo
-- PostGIS, com PK em `srid`) valida o código EPSG no próprio banco; a CLI ainda
-- valida em código para dar mensagem acionável.
--
-- NÃO editar 0001–0006: bancos de dev já as aplicaram e o runner do sqlx
-- rastreia por versão/checksum — editar não re-executa. Esta migração usa
-- `ADD COLUMN IF NOT EXISTS` e o guard de `pg_constraint`, então é idempotente
-- se re-executada manualmente.

ALTER TABLE cell_layers
    ADD COLUMN IF NOT EXISTS srid integer NOT NULL DEFAULT 4674;

COMMENT ON COLUMN cell_layers.srid IS
    'EPSG do SRS em que resolution/origin_x/origin_y são expressos (graus se geográfico, metros se projetado). O armazenamento das células é sempre EPSG:4674.';

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'cell_layers_srid_fkey'
          AND conrelid = 'cell_layers'::regclass
          AND contype = 'f'
    ) THEN
        ALTER TABLE cell_layers
            ADD CONSTRAINT cell_layers_srid_fkey
            FOREIGN KEY (srid) REFERENCES spatial_ref_sys (srid);
    END IF;
END
$$;
