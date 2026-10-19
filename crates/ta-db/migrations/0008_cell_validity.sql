-- 0008_cell_validity.sql — gate de validade geométrica em `cells`.
--
-- `features` já rejeita geometria inválida (`features_geom_validity`, 0002).
-- `cells` não tinha o gate, mas o caminho real de escrita REPROJETA a célula
-- do SRS da grade para EPSG:4674 com `ST_Transform` — e uma reprojeção pode
-- produzir polígono inválido (auto-interseção por arredondamento/rotação).
-- Polígono inválido quebra consultas espaciais (área/intersect) e a Fase 3.
--
-- Política idêntica à de `features`: **rejeitar na escrita com mensagem
-- acionável**, nunca reparar silenciosamente — `ST_MakeValid` é
-- responsabilidade de quem PRODUZ o dado (ingestão/geração), não da
-- persistência.
--
-- NÃO editar 0001–0007: bancos de dev já as aplicaram e o runner do sqlx
-- rastreia por versão/checksum. Idempotente se re-executada manualmente:
-- `CREATE OR REPLACE FUNCTION` no corpo e `DROP TRIGGER IF EXISTS` antes do
-- `CREATE TRIGGER`.

CREATE OR REPLACE FUNCTION cells_reject_invalid_geom() RETURNS trigger AS $$
BEGIN
    IF NOT ST_IsValid(NEW.geom) THEN
        RAISE EXCEPTION
            'geometria de célula inválida rejeitada em cells (cell_layer_id=%, cell_id=%): valide/reprojete na origem (ST_IsValid/ST_MakeValid) antes de gravar — a persistência não repara geometria',
            NEW.cell_layer_id, NEW.cell_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS cells_geom_validity ON cells;
CREATE TRIGGER cells_geom_validity
    BEFORE INSERT OR UPDATE ON cells
    FOR EACH ROW EXECUTE FUNCTION cells_reject_invalid_geom();
