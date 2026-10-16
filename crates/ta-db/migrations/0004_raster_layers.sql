-- 0004_raster_layers.sql — camadas raster por referência (Fase 2).
--
-- O import de raster (ta-cli import-raster) registra a camada SEM copiar
-- pixels: `source_uri` aponta o arquivo (GeoTIFF/COG), `bbox` guarda o
-- envelope derivado do geotransform (reprojetado para 4674 quando preciso).
-- Para isso, `layers.geom_type` passa a aceitar 'raster' (idempotente: o DO
-- block só troca o CHECK se o valor ainda não estiver presente).

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'layers_geom_type_check'
          AND conrelid = 'layers'::regclass
          AND pg_get_constraintdef(oid) LIKE '%raster%'
    ) THEN
        ALTER TABLE layers DROP CONSTRAINT layers_geom_type_check;
        ALTER TABLE layers ADD CONSTRAINT layers_geom_type_check CHECK (geom_type IN (
            'point', 'linestring', 'polygon',
            'multipoint', 'multilinestring', 'multipolygon',
            'geometry', 'raster'));
    END IF;
END
$$;
