-- 0012_queue.sql — fila de processos: `tasks`, `processes` e log por célula.
--
-- `tasks` é a unidade de trabalho enfileirada pelo operador (hoje só
-- `kind = 'classify'`; o CHECK é extensível a novos kinds, mas nenhum outro
-- kind é inventado nesta fase — YAGNI). A task referencia os objetos do fluxo
-- por FK **nullable com SET NULL**: apagar a grade/rascunho/ruleset não apaga
-- o histórico da task, apenas desvincula (o run recusa task sem referência
-- com mensagem acionável).
--
-- `processes` é um processo POR CÉLULA da task. `UNIQUE(task_id, cell_layer_id,
-- cell_id)` torna o enfileiramento **idempotente** (`ON CONFLICT DO NOTHING`):
-- re-enfileirar a mesma task não duplica processo. `status` segue o ciclo
-- pending → running → done | error; `attempts` e `last_error` dão o rastro de
-- retomada (o reprocessamento de `error` é EXPLÍCITO, via `task retry`).
--
-- `cell_log` é o log por célula: `level` info/warn/error com mensagem livre
-- gerada pelo executor da task. É histórico: apagar a task apaga o log
-- (CASCADE); apagar a grade apaga junto (a grade é dona das células).
--
-- Tudo idempotente (`IF NOT EXISTS`); nomes próprios (Decisão 2 do plano).

CREATE TABLE IF NOT EXISTS tasks (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id    UUID NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- Extensível por design; nesta fase só existe 'classify'.
    kind          TEXT NOT NULL CHECK (kind IN ('classify')),
    -- Nome de exibição da task (livre; não é chave).
    name          TEXT NOT NULL,
    -- Parâmetros livres da task (jsonb; '{}' = sem parâmetros).
    params        JSONB NOT NULL DEFAULT '{}',
    -- Referências do fluxo: nullable de propósito — apagar o objeto desvincula
    -- (SET NULL) em vez de apagar o histórico da task.
    cell_layer_id UUID REFERENCES cell_layers(id) ON DELETE SET NULL,
    ruleset_id    UUID REFERENCES rulesets(id) ON DELETE SET NULL,
    draft_layer_id UUID REFERENCES draft_layers(id) ON DELETE SET NULL,
    -- pending → running → done | failed | cancelled (derivado dos processos).
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'running', 'done', 'cancelled', 'failed')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS tasks_project_id_idx ON tasks (project_id);

CREATE TABLE IF NOT EXISTS processes (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id       UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    cell_layer_id UUID NOT NULL REFERENCES cell_layers(id) ON DELETE CASCADE,
    -- Identificador determinístico da célula ("<col>_<row>", mesmo de `cells`).
    cell_id       TEXT NOT NULL,
    -- pending → running → done | error (erro volta só por `task retry`).
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'running', 'done', 'error')),
    attempts      INTEGER NOT NULL DEFAULT 0,
    last_error    TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Um processo por célula por task: torna o enfileiramento idempotente.
    UNIQUE (task_id, cell_layer_id, cell_id)
);

-- Claim de trabalho (`FOR UPDATE SKIP LOCKED`) filtra por task + status.
CREATE INDEX IF NOT EXISTS processes_task_status_idx
    ON processes (task_id, status);

CREATE TABLE IF NOT EXISTS cell_log (
    id            BIGSERIAL PRIMARY KEY,
    task_id       UUID NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    cell_layer_id UUID NOT NULL REFERENCES cell_layers(id) ON DELETE CASCADE,
    cell_id       TEXT NOT NULL,
    level         TEXT NOT NULL CHECK (level IN ('info', 'warn', 'error')),
    message       TEXT NOT NULL,
    created_on    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS cell_log_task_cell_idx
    ON cell_log (task_id, cell_layer_id, cell_id);
