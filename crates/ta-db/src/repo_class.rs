//! Repositórios de **classes**, **grupos de classes** e **camadas de rascunho**.
//!
//! Bind-only: todo valor entra por parâmetro (`$n`); nomes de tabela/coluna são
//! literais fixos no código. As unicidades (`UNIQUE(project_id, name)`) são as
//! mesmas do schema (`0009_classes`).

use async_trait::async_trait;
use sqlx::PgPool;
use ta_core::classification::{Class, ClassGroup, DraftLayer, DraftState};

use crate::error::check_id;
use crate::{Error, Result};

/// Contrato do repositório de grupos de classes.
#[async_trait]
pub trait ClassGroupRepo: Send + Sync {
    /// Insere o grupo; id vazio deixa o banco gerar o UUID.
    async fn create(&self, group: &ClassGroup) -> Result<ClassGroup>;
    /// Busca por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<ClassGroup>>;
    /// Grupos do projeto, em ordem de exibição (`sort_order`, `name`).
    async fn list_for_project(&self, project_id: &str) -> Result<Vec<ClassGroup>>;
    /// Atualiza nome/ordem; `None` = id ausente.
    async fn update(&self, group: &ClassGroup) -> Result<Option<ClassGroup>>;
    /// Remove o grupo (classes ficam sem grupo, `ON DELETE SET NULL`).
    async fn delete(&self, id: &str) -> Result<bool>;
}

/// Contrato do repositório de classes.
#[async_trait]
pub trait ClassRepo: Send + Sync {
    /// Insere a classe; id vazio deixa o banco gerar o UUID.
    async fn create(&self, class: &Class) -> Result<Class>;
    /// Busca por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<Class>>;
    /// Classes do projeto, em ordem de exibição (`sort_order`, `name`).
    async fn list_for_project(&self, project_id: &str) -> Result<Vec<Class>>;
    /// Atualiza nome/grupo/mãe/cor/ordem/atividade; `None` = id ausente.
    async fn update(&self, class: &Class) -> Result<Option<Class>>;
    /// Remove a classe; recusa (`ClassInUseByRules`) enquanto houver regra
    /// apontando para ela (FK `ON DELETE RESTRICT`, migração 0011). A remoção
    /// com regras é caminho explícito: [`PgClassRepo::delete_with_rules`].
    async fn delete(&self, id: &str) -> Result<bool>;
}

/// Contrato do repositório de camadas de rascunho.
///
/// `create`/`list` no trait; as operações de remoção/leitura pontual ficam
/// como métodos inerentes de [`PgDraftLayerRepo`] (o mesmo padrão de
/// `PgTaskRepo`): `get`, `count_cells` e `delete` servem ao pré-voo do
/// `task run` e ao `draft delete` (destrutivo, exige confirmação na CLI).
#[async_trait]
pub trait DraftLayerRepo: Send + Sync {
    /// Insere o rascunho; id vazio deixa o banco gerar o UUID.
    async fn create(&self, layer: &DraftLayer) -> Result<DraftLayer>;
    /// Rascunhos do projeto, em ordem alfabética.
    async fn list_for_project(&self, project_id: &str) -> Result<Vec<DraftLayer>>;
}

type GroupRow = (
    String,               // id
    String,               // project_id
    String,               // name
    i32,                  // sort_order
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn group_from_row(row: GroupRow) -> ClassGroup {
    ClassGroup {
        id: row.0,
        project_id: row.1,
        name: row.2,
        sort_order: row.3,
        created_at: row.4,
        updated_at: row.5,
    }
}

type ClassRow = (
    String,               // id
    String,               // project_id
    Option<String>,       // class_group_id
    Option<String>,       // parent_id
    String,               // name
    Option<String>,       // color
    i32,                  // sort_order
    bool,                 // is_active
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn class_from_row(row: ClassRow) -> Class {
    Class {
        id: row.0,
        project_id: row.1,
        class_group_id: row.2,
        parent_id: row.3,
        name: row.4,
        color: row.5,
        sort_order: row.6,
        is_active: row.7,
        created_at: row.8,
        updated_at: row.9,
    }
}

/// Grupos de classes sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgClassGroupRepo {
    pool: PgPool,
}

impl PgClassGroupRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ClassGroupRepo for PgClassGroupRepo {
    async fn create(&self, group: &ClassGroup) -> Result<ClassGroup> {
        check_id(&group.project_id)?;
        let row: GroupRow = sqlx::query_as(
            "INSERT INTO class_groups (id, project_id, name, sort_order) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3, $4) \
             RETURNING id::text, project_id::text, name, sort_order, created_at, updated_at",
        )
        .bind(&group.id)
        .bind(&group.project_id)
        .bind(&group.name)
        .bind(group.sort_order)
        .fetch_one(&self.pool)
        .await?;
        Ok(group_from_row(row))
    }

    async fn get(&self, id: &str) -> Result<Option<ClassGroup>> {
        check_id(id)?;
        let row: Option<GroupRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, sort_order, created_at, updated_at \
             FROM class_groups WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(group_from_row))
    }

    async fn list_for_project(&self, project_id: &str) -> Result<Vec<ClassGroup>> {
        check_id(project_id)?;
        let rows: Vec<GroupRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, sort_order, created_at, updated_at \
             FROM class_groups WHERE project_id = $1::uuid ORDER BY sort_order, name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(group_from_row).collect())
    }

    async fn update(&self, group: &ClassGroup) -> Result<Option<ClassGroup>> {
        check_id(&group.id)?;
        let row: Option<GroupRow> = sqlx::query_as(
            "UPDATE class_groups SET name = $2, sort_order = $3, updated_at = now() \
             WHERE id = $1::uuid \
             RETURNING id::text, project_id::text, name, sort_order, created_at, updated_at",
        )
        .bind(&group.id)
        .bind(&group.name)
        .bind(group.sort_order)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(group_from_row))
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        let res = sqlx::query("DELETE FROM class_groups WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// Classes sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgClassRepo {
    pool: PgPool,
}

impl PgClassRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Sobe a cadeia de mães a partir de `parent_id` e devolve erro se
    /// `class_id` aparece nela (ciclo) — inclui auto-pai — ou se a cadeia
    /// excede [`MAX_CLASS_DEPTH`] (ciclo provável). Pai inexistente passa:
    /// a FK rejeita, não é ciclo.
    async fn ensure_no_parent_cycle(&self, class_id: &str, parent_id: &str) -> Result<()> {
        let cycle = || Error::ClassParentCycle {
            class_id: class_id.to_owned(),
            parent_id: parent_id.to_owned(),
        };
        if parent_id == class_id {
            return Err(cycle());
        }
        let mut current = parent_id.to_owned();
        for _ in 0..MAX_CLASS_DEPTH {
            let next: Option<Option<String>> =
                sqlx::query_scalar("SELECT parent_id::text FROM classes WHERE id = $1::uuid")
                    .bind(&current)
                    .fetch_optional(&self.pool)
                    .await?;
            match next.flatten() {
                Some(next) if next == class_id => return Err(cycle()),
                Some(next) => current = next,
                None => return Ok(()),
            }
        }
        Err(cycle())
    }

    /// Remove a classe e TODAS as regras que a decidem numa **única
    /// transação** — caminho do `class delete --force`, com intenção
    /// explícita do operador (nunca efeito colateral de uma cascata).
    /// `Ok(false)` = classe ausente.
    pub async fn delete_with_rules(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM rules WHERE class_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let res = sqlx::query("DELETE FROM classes WHERE id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(res.rows_affected() > 0)
    }

    /// Pré-checagem do `class delete` sobre `draft_cells`
    /// (`draft_cells.class_id` é `ON DELETE RESTRICT` na 0013): quantas
    /// células a classe decide em CADA rascunho — `(nome do rascunho,
    /// células nele)`, ordenado por nome. A CLI soma e lista os nomes na
    /// mensagem acionável.
    pub async fn count_drafts_for_class(&self, class_id: &str) -> Result<Vec<(String, i64)>> {
        check_id(class_id)?;
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT dl.name, count(*) \
                 FROM draft_cells dc \
                 JOIN draft_layers dl ON dl.id = dc.draft_layer_id \
                 WHERE dc.class_id = $1::uuid \
                 GROUP BY dl.name ORDER BY dl.name",
        )
        .bind(class_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Caminho do `class delete --force`: numa **única transação** apaga as
    /// `draft_cells` que a classe decide, as regras que a apontam e a
    /// classe — e devolve AS TRÊS CONTAGENS (nada silencioso; ou sai tudo,
    /// ou o rollback não deixa sair nada).
    pub async fn delete_force(&self, id: &str) -> Result<ClassForceDeleted> {
        check_id(id)?;
        let mut tx = self.pool.begin().await?;
        let draft_cells = sqlx::query("DELETE FROM draft_cells WHERE class_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let rules = sqlx::query("DELETE FROM rules WHERE class_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let class = sqlx::query("DELETE FROM classes WHERE id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        tx.commit().await?;
        Ok(ClassForceDeleted {
            draft_cells,
            rules,
            class,
        })
    }
}

/// Contagens do `class delete --force` — as três remoções da transação
/// única, reportadas ao operador (nada silencioso).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassForceDeleted {
    /// Células classificadas removidas (`draft_cells` que a classe decidia).
    pub draft_cells: u64,
    /// Regras removidas.
    pub rules: u64,
    /// `true` se a classe foi apagada (`false` = já não existia).
    pub class: bool,
}

/// Limite de profundidade ao subir a cadeia de `parent_id` (F6): a
/// hierarquia de classes é curta; ultrapassar isso indica ciclo.
const MAX_CLASS_DEPTH: usize = 100;

fn check_optional_id(id: Option<&String>) -> Result<()> {
    if let Some(id) = id {
        check_id(id)?;
    }
    Ok(())
}

/// Violação de FK do Postgres (SQLSTATE 23503).
fn is_foreign_key_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_foreign_key_violation())
}

#[async_trait]
impl ClassRepo for PgClassRepo {
    async fn create(&self, class: &Class) -> Result<Class> {
        check_id(&class.project_id)?;
        check_optional_id(class.class_group_id.as_ref())?;
        check_optional_id(class.parent_id.as_ref())?;
        if let Some(parent_id) = &class.parent_id {
            self.ensure_no_parent_cycle(&class.id, parent_id).await?;
        }
        let row: ClassRow = sqlx::query_as(
            "INSERT INTO classes (id, project_id, class_group_id, parent_id, name, color, \
                                 sort_order, is_active) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3::uuid, \
                     $4::uuid, $5, $6, $7, $8) \
             RETURNING id::text, project_id::text, class_group_id::text, parent_id::text, \
                       name, color, sort_order, is_active, created_at, updated_at",
        )
        .bind(&class.id)
        .bind(&class.project_id)
        .bind(&class.class_group_id)
        .bind(&class.parent_id)
        .bind(&class.name)
        .bind(&class.color)
        .bind(class.sort_order)
        .bind(class.is_active)
        .fetch_one(&self.pool)
        .await?;
        Ok(class_from_row(row))
    }

    async fn get(&self, id: &str) -> Result<Option<Class>> {
        check_id(id)?;
        let row: Option<ClassRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, class_group_id::text, parent_id::text, name, \
                    color, sort_order, is_active, created_at, updated_at \
             FROM classes WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(class_from_row))
    }

    async fn list_for_project(&self, project_id: &str) -> Result<Vec<Class>> {
        check_id(project_id)?;
        let rows: Vec<ClassRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, class_group_id::text, parent_id::text, name, \
                    color, sort_order, is_active, created_at, updated_at \
             FROM classes WHERE project_id = $1::uuid ORDER BY sort_order, name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(class_from_row).collect())
    }

    async fn update(&self, class: &Class) -> Result<Option<Class>> {
        check_id(&class.id)?;
        check_optional_id(class.class_group_id.as_ref())?;
        check_optional_id(class.parent_id.as_ref())?;
        if let Some(parent_id) = &class.parent_id {
            self.ensure_no_parent_cycle(&class.id, parent_id).await?;
        }
        let row: Option<ClassRow> = sqlx::query_as(
            "UPDATE classes SET class_group_id = $2::uuid, parent_id = $3::uuid, name = $4, \
                               color = $5, sort_order = $6, is_active = $7, updated_at = now() \
             WHERE id = $1::uuid \
             RETURNING id::text, project_id::text, class_group_id::text, parent_id::text, \
                       name, color, sort_order, is_active, created_at, updated_at",
        )
        .bind(&class.id)
        .bind(&class.class_group_id)
        .bind(&class.parent_id)
        .bind(&class.name)
        .bind(&class.color)
        .bind(class.sort_order)
        .bind(class.is_active)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(class_from_row))
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        match sqlx::query("DELETE FROM classes WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await
        {
            Ok(res) => Ok(res.rows_affected() > 0),
            // FKs `ON DELETE RESTRICT` (0011 regras; 0013 draft_cells):
            // traduz a recusa do banco em contagem acionável, discriminando
            // QUAL referência recusou pelo nome da constraint (padrão do
            // Postgres `<tabela>_<coluna>_fkey`).
            Err(err) if is_foreign_key_violation(&err) => {
                let constraint = match &err {
                    sqlx::Error::Database(db) => db.constraint().unwrap_or_default(),
                    _ => "",
                };
                if constraint == "draft_cells_class_id_fkey" {
                    let count: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM draft_cells WHERE class_id = $1::uuid",
                    )
                    .bind(id)
                    .fetch_one(&self.pool)
                    .await?;
                    return Err(Error::ClassInUseByDraftCells {
                        class_id: id.to_owned(),
                        count,
                    });
                }
                let count: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM rules WHERE class_id = $1::uuid")
                        .bind(id)
                        .fetch_one(&self.pool)
                        .await?;
                Err(Error::ClassInUseByRules {
                    class_id: id.to_owned(),
                    count,
                })
            }
            Err(err) => Err(err.into()),
        }
    }
}

type DraftRow = (
    String,               // id
    String,               // project_id
    Option<String>,       // phase_id
    Option<String>,       // cell_layer_id
    String,               // name
    String,               // state
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn draft_from_row(row: DraftRow) -> Result<DraftLayer> {
    let state: DraftState = row.5.parse()?;
    Ok(DraftLayer {
        id: row.0,
        project_id: row.1,
        phase_id: row.2,
        cell_layer_id: row.3,
        name: row.4,
        state,
        created_at: row.6,
        updated_at: row.7,
    })
}

/// Camadas de rascunho sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgDraftLayerRepo {
    pool: PgPool,
}

impl PgDraftLayerRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Busca por id (`None` = ausente) — pré-voo do `task run`: o rascunho
    /// da task precisa existir e pertencer à MESMA grade da task.
    pub async fn get(&self, id: &str) -> Result<Option<DraftLayer>> {
        check_id(id)?;
        let row: Option<DraftRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, phase_id::text, cell_layer_id::text, name, \
                    state, created_at, updated_at \
             FROM draft_layers WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(draft_from_row).transpose()
    }

    /// Quantas células classificadas o rascunho guarda — a contagem que o
    /// `draft delete` mostra ANTES de apagar (o rascunho é derivado, mas a
    /// remoção é destrutiva e exige intenção explícita na CLI).
    pub async fn count_cells(&self, draft_layer_id: &str) -> Result<i64> {
        check_id(draft_layer_id)?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM draft_cells WHERE draft_layer_id = $1::uuid")
                .bind(draft_layer_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }

    /// Remove o rascunho; `draft_cells` cai por CASCADE (0013) — a remoção
    /// é sempre EXPLÍCITA na camada de rascunho, nunca efeito colateral de
    /// apagar outro objeto. `Ok(false)` = rascunho ausente.
    pub async fn delete(&self, draft_layer_id: &str) -> Result<bool> {
        check_id(draft_layer_id)?;
        let res = sqlx::query("DELETE FROM draft_layers WHERE id = $1::uuid")
            .bind(draft_layer_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[async_trait]
impl DraftLayerRepo for PgDraftLayerRepo {
    async fn create(&self, layer: &DraftLayer) -> Result<DraftLayer> {
        check_id(&layer.project_id)?;
        check_optional_id(layer.phase_id.as_ref())?;
        check_optional_id(layer.cell_layer_id.as_ref())?;
        let row: DraftRow = sqlx::query_as(
            "INSERT INTO draft_layers (id, project_id, phase_id, cell_layer_id, name, state) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3::uuid, \
                     $4::uuid, $5, $6) \
             RETURNING id::text, project_id::text, phase_id::text, cell_layer_id::text, name, \
                       state, created_at, updated_at",
        )
        .bind(&layer.id)
        .bind(&layer.project_id)
        .bind(&layer.phase_id)
        .bind(&layer.cell_layer_id)
        .bind(&layer.name)
        .bind(layer.state.as_str())
        .fetch_one(&self.pool)
        .await?;
        draft_from_row(row)
    }

    async fn list_for_project(&self, project_id: &str) -> Result<Vec<DraftLayer>> {
        check_id(project_id)?;
        let rows: Vec<DraftRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, phase_id::text, cell_layer_id::text, name, \
                    state, created_at, updated_at \
             FROM draft_layers WHERE project_id = $1::uuid ORDER BY name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(draft_from_row).collect()
    }
}
