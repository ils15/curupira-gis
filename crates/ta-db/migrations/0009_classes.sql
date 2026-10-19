-- 0009_classes.sql — classes, grupos de classes e camadas de rascunho.
--
-- `class_groups` organiza as classes de um projeto (ex.: "vegetação",
-- "água"). `classes` é o rótulo final aplicado a uma célula pela avaliação de
-- regras; pode ter grupo (`class_group_id`, SET NULL ao apagar o grupo) e
-- classe-mãe (`parent_id`, hierarquia de até N níveis, SET NULL ao apagar a
-- mãe). `color` é exibição (`#RRGGBB`), nunca regra de domínio.
--
-- `draft_layers` agrupa a classificação em andamento de uma fase e/ou de uma
-- camada de células. `state = 'consolidated'` marca o resultado definitivo.
-- Apagar a grade apaga o rascunho (CASCADE); apagar a fase apenas desvincula.
--
-- Tudo idempotente (`IF NOT EXISTS`); nomes próprios (Decisão 2 do plano).

CREATE TABLE IF NOT EXISTS class_groups (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE IF NOT EXISTS classes (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id     UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    class_group_id UUID REFERENCES class_groups(id) ON DELETE SET NULL,
    parent_id      UUID REFERENCES classes(id) ON DELETE SET NULL,
    name           TEXT NOT NULL,
    -- Cor de exibição (#RRGGBB); NULL = sem cor definida.
    color          TEXT,
    sort_order     INTEGER NOT NULL DEFAULT 0,
    is_active      BOOLEAN NOT NULL DEFAULT true,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name),
    CONSTRAINT classes_color_hex CHECK (
        color IS NULL OR color ~ '^#[0-9A-Fa-f]{6}$'
    )
);

CREATE INDEX IF NOT EXISTS classes_project_group_idx
    ON classes (project_id, class_group_id);

CREATE TABLE IF NOT EXISTS draft_layers (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id    UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    phase_id      UUID REFERENCES phases(id) ON DELETE SET NULL,
    cell_layer_id UUID REFERENCES cell_layers(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    state         TEXT NOT NULL DEFAULT 'draft'
                  CHECK (state IN ('draft', 'consolidated')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE INDEX IF NOT EXISTS draft_layers_phase_id_idx
    ON draft_layers (phase_id);
CREATE INDEX IF NOT EXISTS draft_layers_cell_layer_id_idx
    ON draft_layers (cell_layer_id);
