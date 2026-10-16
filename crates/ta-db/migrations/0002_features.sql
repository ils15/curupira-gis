-- 0002_features.sql — feições vetoriais (geometria + atributos livres).
--
-- Política de validação (decisão Gate 1):
-- - A inserção **valida** a geometria com ST_IsValid (trigger abaixo) e
--   **rejeita a inválida com erro claro** — o clean-room não repara
--   silenciosamente; reparo (ST_MakeValid) é responsabilidade do
--   ta-gis::normalizer, na ingestão, nunca na persistência.
-- - O SRID é enforce em duas camadas: typmod `geometry(Geometry, 4674)`
--   (rejeita SRID != 4674 no decode do PostGIS) e o CHECK redundante
--   `features_srid_4674` (protege contra recargas/dumps fora do typmod).
-- - Spatial index: GIST em geom, usado por FeatureRepo::bbox_query (operador &&).

CREATE TABLE features (
    id       BIGSERIAL PRIMARY KEY,
    layer_id UUID NOT NULL REFERENCES layers(id) ON DELETE CASCADE,
    geom     GEOMETRY(Geometry, 4674) NOT NULL,
    attrs    JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX features_geom_gist ON features USING GIST (geom);

ALTER TABLE features
    ADD CONSTRAINT features_srid_4674 CHECK (ST_SRID(geom) = 4674);

-- Rejeita geometria inválida na inserção/atualização com mensagem explícita
-- (aponta o reparo para a origem, mantendo a persistência passiva).
CREATE OR REPLACE FUNCTION features_reject_invalid_geom() RETURNS trigger AS $$
BEGIN
    IF NOT ST_IsValid(NEW.geom) THEN
        RAISE EXCEPTION
            'geometria inválida rejeitada em features (layer_id=%): rode ST_IsValid/ST_MakeValid na ingestão (política: validar na inserção)',
            NEW.layer_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER features_geom_validity
    BEFORE INSERT OR UPDATE ON features
    FOR EACH ROW EXECUTE FUNCTION features_reject_invalid_geom();
