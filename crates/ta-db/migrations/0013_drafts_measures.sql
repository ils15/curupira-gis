-- 0013_drafts_measures.sql — resultado da classificação e medidas por célula.
--
-- `cell_measures` guarda as medidas observadas por célula (`ndvi`, `nbr`, …),
-- uma linha por (célula, medida) com `CHECK(isfinite(value))` — NaN/∞ não são
-- observação. `ON CONFLICT DO UPDATE` (upsert) torna `measure set`
-- idempotente; `source` é a proveniência livre (ex.: cena/índice de origem).
--
-- `draft_cells` é o resultado da classificação dentro de um rascunho: a classe
-- decidida por qual ruleset, com `decided_on`. Reclassificar a mesma célula no
-- MESMO rascunho é upsert (não duplica). Proveniência de apagamento coerente
-- com a decisão da Fase 2 (`0011_rule_class_restrict`): apagar classe ou
-- ruleset que decide um rascunho é RECUSADO (RESTRICT) — nada de apagar
-- classificação em silêncio; o caminho explícito é apagar o rascunho.
--
-- `rulesets.is_active` marca O ruleset ativo do projeto (o default de
-- `task create classify`); o índice parcial único garante **no máximo um**
-- ativo por projeto, inclusive para escritores fora do repositório (SQL
-- direto).
--
-- **Integridade multi-projeto (F5, registrado na revisão da Fase 2):**
-- triggers `BEFORE INSERT/UPDATE` recusam referência cruzada de projetos —
-- `rules.class_id` fora do projeto do ruleset, `classes.class_group_id`/
-- `classes.parent_id` fora do projeto da classe e `draft_cells` apontando
-- objeto de outro projeto — com mensagem acionável. A fila de processos
-- escreve em lote direto no banco; a garantia final é do banco, não do
-- repositório.
--
-- NÃO editar 0001–0012. Idempotente (`IF NOT EXISTS`/`DROP TRIGGER IF EXISTS`
-- + `CREATE OR REPLACE FUNCTION`).

CREATE TABLE IF NOT EXISTS cell_measures (
    cell_layer_id UUID NOT NULL,
    cell_id       TEXT NOT NULL,
    -- Nome da medida, mesma gramática dos predicados (ex.: `ndvi`).
    measure       TEXT NOT NULL CHECK (length(btrim(measure)) > 0),
    -- `isfinite()` do PG é só para timestamp/interval; para float8, NaN ordena
    -- acima de +Infinity, então `< Infinity AND > -Infinity` exclui NaN e ±inf.
    value         DOUBLE PRECISION NOT NULL
                  CHECK (value < 'Infinity'::float8 AND value > '-Infinity'::float8),
    -- Proveniência livre (ex.: cena, índice, script de origem).
    source        TEXT,
    updated_on    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (cell_layer_id, cell_id, measure),
    FOREIGN KEY (cell_layer_id, cell_id)
        REFERENCES cells (cell_layer_id, cell_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS draft_cells (
    draft_layer_id UUID NOT NULL REFERENCES draft_layers(id) ON DELETE CASCADE,
    cell_layer_id  UUID NOT NULL,
    cell_id        TEXT NOT NULL,
    -- Classe decidida: RESTRICT — apagar classe não apaga classificação em
    -- silêncio (mesma política de `rules.class_id` na 0011).
    class_id       UUID NOT NULL REFERENCES classes(id) ON DELETE RESTRICT,
    -- Ruleset de proveniência: RESTRICT — o rascunho sobrevive ao ruleset.
    ruleset_id     UUID NOT NULL REFERENCES rulesets(id) ON DELETE RESTRICT,
    decided_on     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (draft_layer_id, cell_layer_id, cell_id),
    FOREIGN KEY (cell_layer_id, cell_id)
        REFERENCES cells (cell_layer_id, cell_id) ON DELETE CASCADE
);

-- Consulta por célula (GUI/CLI) sem varrer o rascunho inteiro.
CREATE INDEX IF NOT EXISTS draft_cells_cell_idx
    ON draft_cells (cell_layer_id, cell_id);

-- O ruleset ativo do projeto: default do enfileiramento (`task create`).
ALTER TABLE rulesets
    ADD COLUMN IF NOT EXISTS is_active BOOLEAN NOT NULL DEFAULT false;

-- No máximo UM ruleset ativo por projeto — garantia no banco, inclusive para
-- escritores fora do repositório (SQL direto).
DROP INDEX IF EXISTS rulesets_one_active_per_project_idx;
CREATE UNIQUE INDEX IF NOT EXISTS rulesets_one_active_per_project_idx
    ON rulesets (project_id)
    WHERE is_active;

-- ---------------------------------------------------------------------------
-- F5 — gatilhos de integridade multi-projeto (INSERT/UPDATE).
-- ---------------------------------------------------------------------------

-- `rules.class_id` deve pertencer ao projeto do `rulesets` da regra.
CREATE OR REPLACE FUNCTION rules_reject_cross_project_class() RETURNS trigger AS $$
DECLARE
    ruleset_project UUID;
    class_project   UUID;
BEGIN
    SELECT rs.project_id, c.project_id
      INTO ruleset_project, class_project
      FROM rulesets rs
      JOIN classes c ON c.id = NEW.class_id
     WHERE rs.id = NEW.ruleset_id;
    IF class_project IS DISTINCT FROM ruleset_project THEN
        RAISE EXCEPTION
            'regra cross-project rejeitada em rules: a classe % pertence ao projeto %, mas o ruleset % pertence ao projeto %; use uma classe do mesmo projeto do ruleset',
            NEW.class_id, class_project, NEW.ruleset_id, ruleset_project
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS rules_class_project_chk ON rules;
CREATE TRIGGER rules_class_project_chk
    BEFORE INSERT OR UPDATE ON rules
    FOR EACH ROW EXECUTE FUNCTION rules_reject_cross_project_class();

-- `classes.class_group_id` e `classes.parent_id` devem pertencer ao projeto da
-- classe (grupo e mãe de outro projeto não são exibíveis nem decidíveis).
CREATE OR REPLACE FUNCTION classes_reject_cross_project_refs() RETURNS trigger AS $$
DECLARE
    group_project  UUID;
    parent_project UUID;
BEGIN
    IF NEW.class_group_id IS NOT NULL THEN
        SELECT project_id INTO group_project
          FROM class_groups WHERE id = NEW.class_group_id;
        IF group_project IS DISTINCT FROM NEW.project_id THEN
            RAISE EXCEPTION
                'classe cross-project rejeitada em classes: o grupo % pertence ao projeto %, mas a classe % pertence ao projeto %; use um grupo do mesmo projeto',
                NEW.class_group_id, group_project, NEW.id, NEW.project_id
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    IF NEW.parent_id IS NOT NULL THEN
        SELECT project_id INTO parent_project
          FROM classes WHERE id = NEW.parent_id;
        IF parent_project IS DISTINCT FROM NEW.project_id THEN
            RAISE EXCEPTION
                'classe cross-project rejeitada em classes: a classe-mãe % pertence ao projeto %, mas a classe % pertence ao projeto %; use uma mãe do mesmo projeto',
                NEW.parent_id, parent_project, NEW.id, NEW.project_id
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS classes_refs_project_chk ON classes;
CREATE TRIGGER classes_refs_project_chk
    BEFORE INSERT OR UPDATE ON classes
    FOR EACH ROW EXECUTE FUNCTION classes_reject_cross_project_refs();

-- `draft_cells` deve referenciar classe/ruleset/grade do MESMO projeto do
-- rascunho — é o caminho de escrita em lote da fila de classificação.
CREATE OR REPLACE FUNCTION draft_cells_reject_cross_project_refs() RETURNS trigger AS $$
DECLARE
    draft_project  UUID;
    class_project  UUID;
    ruleset_project UUID;
    layer_project  UUID;
BEGIN
    SELECT project_id INTO draft_project FROM draft_layers WHERE id = NEW.draft_layer_id;
    SELECT project_id INTO class_project FROM classes WHERE id = NEW.class_id;
    SELECT project_id INTO ruleset_project FROM rulesets WHERE id = NEW.ruleset_id;
    SELECT project_id INTO layer_project FROM cell_layers WHERE id = NEW.cell_layer_id;
    IF class_project IS DISTINCT FROM draft_project THEN
        RAISE EXCEPTION
            'draft_cells cross-project rejeitado: a classe % pertence ao projeto %, mas o rascunho % pertence ao projeto %',
            NEW.class_id, class_project, NEW.draft_layer_id, draft_project
            USING ERRCODE = 'check_violation';
    END IF;
    IF ruleset_project IS DISTINCT FROM draft_project THEN
        RAISE EXCEPTION
            'draft_cells cross-project rejeitado: o ruleset % pertence ao projeto %, mas o rascunho % pertence ao projeto %',
            NEW.ruleset_id, ruleset_project, NEW.draft_layer_id, draft_project
            USING ERRCODE = 'check_violation';
    END IF;
    IF layer_project IS DISTINCT FROM draft_project THEN
        RAISE EXCEPTION
            'draft_cells cross-project rejeitado: a camada de células % pertence ao projeto %, mas o rascunho % pertence ao projeto %',
            NEW.cell_layer_id, layer_project, NEW.draft_layer_id, draft_project
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS draft_cells_refs_project_chk ON draft_cells;
CREATE TRIGGER draft_cells_refs_project_chk
    BEFORE INSERT OR UPDATE ON draft_cells
    FOR EACH ROW EXECUTE FUNCTION draft_cells_reject_cross_project_refs();
