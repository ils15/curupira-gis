//! Repositórios de **rulesets versionados** (`rulesets`) e **regras** (`rules`).
//!
//! - `PgRuleSetRepo::create` calcula a **próxima versão** da linhagem
//!   `(project_id, name)` com `INSERT … SELECT COALESCE(MAX(version), 0) + 1`
//!   sob `pg_advisory_xact_lock(hashtext(project_id || ':' || name))`: escritas
//!   da mesma linhagem são serializadas na transação (SELECT MAX + INSERT sem
//!   janela de corrida; o lock é solto no commit/rollback). Violação residual
//!   do `UNIQUE(project_id, name, version)` vira
//!   [`crate::Error::VersionRace`] — tente novamente, não é nome duplicado.
//! - `PgRuleRepo` valida que a classe pertence ao **mesmo projeto** do ruleset
//!   (erro tipado [`crate::Error::ClassProjectMismatch`]), conta as regras que
//!   decidem uma classe (pré-checagem de `class delete`) e faz update
//!   **parcial** (`update_partial` — só os campos presentes no patch).
//!
//! O predicado **não** é validado aqui: `ta-db` não depende de `ta-engine` em
//! produção. A CLI valida com `ta_engine::parse_predicate` antes do insert.
//! Bind-only em todas as queries; os SQL são literais fixos.

use async_trait::async_trait;
use sqlx::PgPool;
use ta_core::rule::{Rule, RuleSet};

use crate::error::check_id;
use crate::{Error, Result};

/// Contrato do repositório de rulesets.
#[async_trait]
pub trait RuleSetRepo: Send + Sync {
    /// Cria a **próxima versão** do ruleset `(project_id, name)`.
    async fn create(&self, ruleset: &RuleSet) -> Result<RuleSet>;
    /// Busca por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<RuleSet>>;
    /// Busca por `(project_id, name)`; `version = None` devolve a mais recente.
    async fn find(
        &self,
        project_id: &str,
        name: &str,
        version: Option<i32>,
    ) -> Result<Option<RuleSet>>;
    /// Rulesets do projeto, por nome e versão decrescente.
    async fn list_for_project(&self, project_id: &str) -> Result<Vec<RuleSet>>;
    /// Atualiza a descrição; `None` = id ausente.
    async fn update(&self, ruleset: &RuleSet) -> Result<Option<RuleSet>>;
    /// Remove o ruleset (regras caem em cascata).
    async fn delete(&self, id: &str) -> Result<bool>;
    /// Torna este ruleset o **ativo** do projeto, desativando os demais
    /// (o índice parcial único da `0013` garante no máximo um ativo).
    async fn activate(&self, id: &str) -> Result<Option<RuleSet>>;
    /// O ruleset **ativo** do projeto, se houver (default do `task create`).
    async fn find_active(&self, project_id: &str) -> Result<Option<RuleSet>>;
}

/// Alterações **parciais** de uma regra — apenas os campos presentes
/// (`Some`) são gravados; ausentes (`None`) preservam o valor atual.
///
/// `name` não é patchável: ele identifica a regra dentro do ruleset
/// (`rule update --name <n>`), e renomear mudaria a chave de desempate sem
/// ter sido pedido.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RulePatch {
    /// Classe decidida (id).
    pub class_id: Option<String>,
    /// Prioridade (maior vence).
    pub priority: Option<i32>,
    /// Predicado na gramática do motor.
    pub predicate: Option<String>,
    /// Regra ativa/inativa.
    pub is_active: Option<bool>,
}

impl RulePatch {
    /// `true` quando nenhum campo foi pedido (update vazio é erro na CLI).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.class_id.is_none()
            && self.priority.is_none()
            && self.predicate.is_none()
            && self.is_active.is_none()
    }
}

/// Contrato do repositório de regras.
#[async_trait]
pub trait RuleRepo: Send + Sync {
    /// Insere a regra, validando que a classe é do mesmo projeto do ruleset.
    async fn create(&self, rule: &Rule) -> Result<Rule>;
    /// Busca por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<Rule>>;
    /// Regras do ruleset, maior prioridade primeiro e nome como desempate.
    async fn list_for_ruleset(&self, ruleset_id: &str) -> Result<Vec<Rule>>;
    /// Atualiza nome/prioridade/predicado/classe/atividade.
    async fn update(&self, rule: &Rule) -> Result<Option<Rule>>;
    /// Atualiza **apenas** os campos presentes no patch, identificados por
    /// `(ruleset_id, name)`; `None` = regra ausente. Validando projeto quando
    /// a classe muda.
    async fn update_partial(
        &self,
        ruleset_id: &str,
        name: &str,
        patch: &RulePatch,
    ) -> Result<Option<Rule>>;
    /// Remove a regra. `Ok(false)` = ausente.
    async fn delete(&self, id: &str) -> Result<bool>;
    /// Quantas regras decidem a classe (pré-checagem de `class delete`).
    async fn count_for_class(&self, class_id: &str) -> Result<i64>;
}

type RuleSetRow = (
    String,               // id
    String,               // project_id
    String,               // name
    i32,                  // version
    Option<String>,       // description
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

/// Contagens do `ruleset delete --force` — as três remoções da transação
/// única, reportadas ao operador (nada silencioso).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleSetForceDeleted {
    /// Células classificadas removidas (`draft_cells` que o ruleset decidiu).
    pub draft_cells: u64,
    /// Regras removidas.
    pub rules: u64,
    /// `true` se o ruleset foi apagado (`false` = já não existia).
    pub ruleset: bool,
}

fn ruleset_from_row(row: RuleSetRow) -> RuleSet {
    RuleSet {
        id: row.0,
        project_id: row.1,
        name: row.2,
        version: row.3,
        description: row.4,
        created_at: row.5,
        updated_at: row.6,
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_unique_violation())
}

fn is_foreign_key_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.is_foreign_key_violation())
}

/// Rulesets sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgRuleSetRepo {
    pool: PgPool,
}

impl PgRuleSetRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Import do exchange: INSERT com versão EXPLÍCITA (a linhagem foi
    /// exportada; recriá-la com `MAX(version)+1` por nome perderia buracos de
    /// versão). Executa sobre o **executor** dado — transação da seção.
    /// `(project_id, name, version)` duplicado é recusado pelo banco.
    pub async fn insert_with_version_on<'e, E>(exec: E, ruleset: &RuleSet) -> Result<RuleSet>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        check_id(&ruleset.project_id)?;
        if ruleset.version <= 0 {
            return Err(Error::InvalidId(format!(
                "versão de ruleset deve ser > 0 (recebido {} para `{}`)",
                ruleset.version, ruleset.name
            )));
        }
        let row: RuleSetRow = sqlx::query_as(
            "INSERT INTO rulesets (id, project_id, name, version, description, is_active) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3, $4, $5, \
                     false) \
             RETURNING id::text, project_id::text, name, version, description, \
                       created_at, updated_at",
        )
        .bind(&ruleset.id)
        .bind(&ruleset.project_id)
        .bind(&ruleset.name)
        .bind(ruleset.version)
        .bind(&ruleset.description)
        .fetch_one(exec)
        .await?;
        Ok(ruleset_from_row(row))
    }

    /// `activate` sobre a **conexão** dada (de um pool ou transação): desativa
    /// os demais do projeto e ativa o escolhido, na mesma transação do
    /// chamador — o índice parcial único da `0013` nunca é desafiado.
    pub async fn activate_on(exec: &mut sqlx::PgConnection, id: &str) -> Result<Option<RuleSet>> {
        check_id(id)?;
        sqlx::query(
            "UPDATE rulesets SET is_active = false \
             WHERE project_id = (SELECT project_id FROM rulesets WHERE id = $1::uuid) \
               AND is_active",
        )
        .bind(id)
        .execute(&mut *exec)
        .await?;
        let row: Option<RuleSetRow> = sqlx::query_as(
            "UPDATE rulesets SET is_active = true, updated_at = now() \
             WHERE id = $1::uuid \
             RETURNING id::text, project_id::text, name, version, description, \
                       created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(exec)
        .await?;
        Ok(row.map(ruleset_from_row))
    }

    /// Pré-checagem do `ruleset delete` sobre `draft_cells`
    /// (`draft_cells.ruleset_id` é `ON DELETE RESTRICT` na 0013): quantas
    /// células o ruleset decide em CADA rascunho — `(nome do rascunho,
    /// células nele)`, ordenado por nome.
    pub async fn count_drafts_for_ruleset(&self, ruleset_id: &str) -> Result<Vec<(String, i64)>> {
        check_id(ruleset_id)?;
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT dl.name, count(*) \
                 FROM draft_cells dc \
                 JOIN draft_layers dl ON dl.id = dc.draft_layer_id \
                 WHERE dc.ruleset_id = $1::uuid \
                 GROUP BY dl.name ORDER BY dl.name",
        )
        .bind(ruleset_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Caminho do `ruleset delete --force`: numa **única transação** apaga
    /// as `draft_cells` que o ruleset decidiu, as regras e o ruleset — e
    /// devolve AS TRÊS CONTAGENS (ou sai tudo, ou o rollback não deixa sair
    /// nada).
    pub async fn delete_force(&self, id: &str) -> Result<RuleSetForceDeleted> {
        check_id(id)?;
        let mut tx = self.pool.begin().await?;
        let draft_cells = sqlx::query("DELETE FROM draft_cells WHERE ruleset_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let rules = sqlx::query("DELETE FROM rules WHERE ruleset_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let ruleset = sqlx::query("DELETE FROM rulesets WHERE id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        tx.commit().await?;
        Ok(RuleSetForceDeleted {
            draft_cells,
            rules,
            ruleset,
        })
    }

    /// Executa o INSERT que calcula `MAX(version)+1` dentro de uma transação
    /// protegida pelo lock consultivo da linhagem.
    ///
    /// `pg_advisory_xact_lock(hashtext(...))` serializa escritas do mesmo
    /// `(project_id, name)` até o fim da transação: o SELECT MAX e o INSERT
    /// enxergam um estado estável, sem janela de corrida entre eles. Escritas
    /// de linhagens diferentes não se bloqueiam (o lock é por chave).
    async fn insert_next_version(&self, ruleset: &RuleSet) -> Result<RuleSetRow> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("{}:{}", ruleset.project_id, ruleset.name))
            .execute(&mut *tx)
            .await?;
        let row: RuleSetRow = sqlx::query_as(
            "INSERT INTO rulesets (id, project_id, name, version, description) \
             SELECT COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3, \
                    COALESCE(MAX(version), 0) + 1, $4 \
             FROM rulesets WHERE project_id = $2::uuid AND name = $3 \
             RETURNING id::text, project_id::text, name, version, description, \
                       created_at, updated_at",
        )
        .bind(&ruleset.id)
        .bind(&ruleset.project_id)
        .bind(&ruleset.name)
        .bind(&ruleset.description)
        .fetch_one(&mut *tx)
        .await
        .map_err(|err| {
            // Com o lock, colisão de versão só sobra de escritor fora do
            // repositório (SQL direto): erro tipado de corrida, não de nome.
            if is_unique_violation(&err) {
                Error::VersionRace {
                    name: ruleset.name.clone(),
                }
            } else {
                Error::from(err)
            }
        })?;
        tx.commit().await?;
        Ok(row)
    }
}

#[async_trait]
impl RuleSetRepo for PgRuleSetRepo {
    async fn create(&self, ruleset: &RuleSet) -> Result<RuleSet> {
        check_id(&ruleset.project_id)?;
        // O lock de linhagem serializa as escritas do mesmo nome; sem retry.
        self.insert_next_version(ruleset)
            .await
            .map(ruleset_from_row)
    }

    async fn get(&self, id: &str) -> Result<Option<RuleSet>> {
        check_id(id)?;
        let row: Option<RuleSetRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, version, description, \
                    created_at, updated_at \
             FROM rulesets WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(ruleset_from_row))
    }

    async fn find(
        &self,
        project_id: &str,
        name: &str,
        version: Option<i32>,
    ) -> Result<Option<RuleSet>> {
        check_id(project_id)?;
        let row: Option<RuleSetRow> = match version {
            Some(version) => {
                sqlx::query_as(
                    "SELECT id::text, project_id::text, name, version, description, \
                            created_at, updated_at \
                     FROM rulesets \
                     WHERE project_id = $1::uuid AND name = $2 AND version = $3",
                )
                .bind(project_id)
                .bind(name)
                .bind(version)
                .fetch_optional(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as(
                    "SELECT id::text, project_id::text, name, version, description, \
                            created_at, updated_at \
                     FROM rulesets \
                     WHERE project_id = $1::uuid AND name = $2 \
                     ORDER BY version DESC LIMIT 1",
                )
                .bind(project_id)
                .bind(name)
                .fetch_optional(&self.pool)
                .await?
            }
        };
        Ok(row.map(ruleset_from_row))
    }

    async fn list_for_project(&self, project_id: &str) -> Result<Vec<RuleSet>> {
        check_id(project_id)?;
        let rows: Vec<RuleSetRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, version, description, \
                    created_at, updated_at \
             FROM rulesets WHERE project_id = $1::uuid ORDER BY name, version DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(ruleset_from_row).collect())
    }

    async fn update(&self, ruleset: &RuleSet) -> Result<Option<RuleSet>> {
        check_id(&ruleset.id)?;
        let row: Option<RuleSetRow> = sqlx::query_as(
            "UPDATE rulesets SET description = $2, updated_at = now() \
             WHERE id = $1::uuid \
             RETURNING id::text, project_id::text, name, version, description, \
                       created_at, updated_at",
        )
        .bind(&ruleset.id)
        .bind(&ruleset.description)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(ruleset_from_row))
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        match sqlx::query("DELETE FROM rulesets WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await
        {
            Ok(res) => Ok(res.rows_affected() > 0),
            // FK `draft_cells.ruleset_id ON DELETE RESTRICT` (0013): o
            // rascunho sobrevive ao ruleset — a recusa do banco vira contagem
            // acionável, não erro cru de SQL.
            Err(err) if is_foreign_key_violation(&err) => {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM draft_cells WHERE ruleset_id = $1::uuid",
                )
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
                Err(Error::RuleSetInUseByDraftCells {
                    ruleset_id: id.to_owned(),
                    count,
                })
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn activate(&self, id: &str) -> Result<Option<RuleSet>> {
        check_id(id)?;
        // Desativa os demais e ativa o escolhido na MESMA transação: o índice
        // parcial único (`rulesets_one_active_per_project_idx`, 0013) nunca é
        // desafiado. Task cancelada no meio = rollback dos dois passos.
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE rulesets SET is_active = false \
             WHERE project_id = (SELECT project_id FROM rulesets WHERE id = $1::uuid) \
               AND is_active",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let row: Option<RuleSetRow> = sqlx::query_as(
            "UPDATE rulesets SET is_active = true, updated_at = now() \
             WHERE id = $1::uuid \
             RETURNING id::text, project_id::text, name, version, description, \
                       created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row.map(ruleset_from_row))
    }

    async fn find_active(&self, project_id: &str) -> Result<Option<RuleSet>> {
        check_id(project_id)?;
        let row: Option<RuleSetRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, version, description, \
                    created_at, updated_at \
             FROM rulesets WHERE project_id = $1::uuid AND is_active \
             ORDER BY version DESC LIMIT 1",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(ruleset_from_row))
    }
}

type RuleRow = (
    String,               // id
    String,               // ruleset_id
    String,               // class_id
    String,               // name
    i32,                  // priority
    String,               // predicate
    bool,                 // is_active
    time::OffsetDateTime, // created_at
    time::OffsetDateTime, // updated_at
);

fn rule_from_row(row: RuleRow) -> Rule {
    Rule {
        id: row.0,
        ruleset_id: row.1,
        class_id: row.2,
        name: row.3,
        priority: row.4,
        predicate: row.5,
        is_active: row.6,
        created_at: row.7,
        updated_at: row.8,
    }
}

/// Regras sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgRuleRepo {
    pool: PgPool,
}

impl PgRuleRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `create` sobre o **executor** dado (pool ou transação), SEM a
    /// pré-checagem de projeto (que é pool-bound) — o import do exchange já
    /// validou o par classe/ruleset por nome; o gatilho F5 da `0013` é o
    /// backstop no banco.
    pub async fn create_on<'e, E>(exec: E, rule: &Rule) -> Result<Rule>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        check_id(&rule.ruleset_id)?;
        check_id(&rule.class_id)?;
        let row: RuleRow = sqlx::query_as(
            "INSERT INTO rules (id, ruleset_id, class_id, name, priority, predicate, is_active) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3::uuid, \
                     $4, $5, $6, $7) \
             RETURNING id::text, ruleset_id::text, class_id::text, name, priority, \
                       predicate, is_active, created_at, updated_at",
        )
        .bind(&rule.id)
        .bind(&rule.ruleset_id)
        .bind(&rule.class_id)
        .bind(&rule.name)
        .bind(rule.priority)
        .bind(&rule.predicate)
        .bind(rule.is_active)
        .fetch_one(exec)
        .await?;
        Ok(rule_from_row(row))
    }

    /// Valida que `class_id` pertence ao projeto do ruleset e devolve o
    /// `project_id` do ruleset (já normalizado pelo banco).
    async fn project_for_class(&self, ruleset_id: &str, class_id: &str) -> Result<String> {
        check_id(ruleset_id)?;
        check_id(class_id)?;
        let ruleset_project: Option<String> =
            sqlx::query_scalar("SELECT project_id::text FROM rulesets WHERE id = $1::uuid")
                .bind(ruleset_id)
                .fetch_optional(&self.pool)
                .await?;
        let Some(ruleset_project) = ruleset_project else {
            return Err(Error::MissingParent {
                kind: "ruleset",
                id: ruleset_id.to_owned(),
            });
        };
        let class_project: Option<String> =
            sqlx::query_scalar("SELECT project_id::text FROM classes WHERE id = $1::uuid")
                .bind(class_id)
                .fetch_optional(&self.pool)
                .await?;
        let Some(class_project) = class_project else {
            return Err(Error::MissingParent {
                kind: "classe",
                id: class_id.to_owned(),
            });
        };
        if class_project != ruleset_project {
            return Err(Error::ClassProjectMismatch {
                class_id: class_id.to_owned(),
                class_project,
                ruleset_project,
            });
        }
        Ok(ruleset_project)
    }

    /// Quantas regras o ruleset tem — a contagem que o `ruleset delete`
    /// reporta (as regras caem em cascata com o ruleset; a remoção é
    /// reportada, nunca silenciosa).
    pub async fn count_for_ruleset(&self, ruleset_id: &str) -> Result<i64> {
        check_id(ruleset_id)?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM rules WHERE ruleset_id = $1::uuid")
                .bind(ruleset_id)
                .fetch_one(&self.pool)
                .await?;
        Ok(count)
    }
}

#[async_trait]
impl RuleRepo for PgRuleRepo {
    async fn create(&self, rule: &Rule) -> Result<Rule> {
        self.project_for_class(&rule.ruleset_id, &rule.class_id)
            .await?;
        let row: RuleRow = sqlx::query_as(
            "INSERT INTO rules (id, ruleset_id, class_id, name, priority, predicate, is_active) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3::uuid, \
                     $4, $5, $6, $7) \
             RETURNING id::text, ruleset_id::text, class_id::text, name, priority, \
                       predicate, is_active, created_at, updated_at",
        )
        .bind(&rule.id)
        .bind(&rule.ruleset_id)
        .bind(&rule.class_id)
        .bind(&rule.name)
        .bind(rule.priority)
        .bind(&rule.predicate)
        .bind(rule.is_active)
        .fetch_one(&self.pool)
        .await?;
        Ok(rule_from_row(row))
    }

    async fn get(&self, id: &str) -> Result<Option<Rule>> {
        check_id(id)?;
        let row: Option<RuleRow> = sqlx::query_as(
            "SELECT id::text, ruleset_id::text, class_id::text, name, priority, \
                    predicate, is_active, created_at, updated_at \
             FROM rules WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(rule_from_row))
    }

    async fn list_for_ruleset(&self, ruleset_id: &str) -> Result<Vec<Rule>> {
        check_id(ruleset_id)?;
        let rows: Vec<RuleRow> = sqlx::query_as(
            "SELECT id::text, ruleset_id::text, class_id::text, name, priority, \
                    predicate, is_active, created_at, updated_at \
             FROM rules WHERE ruleset_id = $1::uuid ORDER BY priority DESC, name",
        )
        .bind(ruleset_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(rule_from_row).collect())
    }

    async fn update(&self, rule: &Rule) -> Result<Option<Rule>> {
        check_id(&rule.id)?;
        self.project_for_class(&rule.ruleset_id, &rule.class_id)
            .await?;
        let row: Option<RuleRow> = sqlx::query_as(
            "UPDATE rules SET class_id = $2::uuid, name = $3, priority = $4, \
                             predicate = $5, is_active = $6, updated_at = now() \
             WHERE id = $1::uuid \
             RETURNING id::text, ruleset_id::text, class_id::text, name, priority, \
                       predicate, is_active, created_at, updated_at",
        )
        .bind(&rule.id)
        .bind(&rule.class_id)
        .bind(&rule.name)
        .bind(rule.priority)
        .bind(&rule.predicate)
        .bind(rule.is_active)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(rule_from_row))
    }

    async fn update_partial(
        &self,
        ruleset_id: &str,
        name: &str,
        patch: &RulePatch,
    ) -> Result<Option<Rule>> {
        check_id(ruleset_id)?;
        // Classe trocada → mesma validação de projeto do `create`.
        if let Some(class_id) = &patch.class_id {
            self.project_for_class(ruleset_id, class_id).await?;
        }
        // COALESCE($n, coluna) grava só o que veio em `Some` — update parcial
        // real: campo ausente preserva o valor atual (SQL literal fixo,
        // bind-only).
        let row: Option<RuleRow> = sqlx::query_as(
            "UPDATE rules SET \
                class_id = COALESCE($3::uuid, class_id), \
                priority = COALESCE($4, priority), \
                predicate = COALESCE($5, predicate), \
                is_active = COALESCE($6, is_active), \
                updated_at = now() \
             WHERE ruleset_id = $1::uuid AND name = $2 \
             RETURNING id::text, ruleset_id::text, class_id::text, name, priority, \
                       predicate, is_active, created_at, updated_at",
        )
        .bind(ruleset_id)
        .bind(name)
        .bind(&patch.class_id)
        .bind(patch.priority)
        .bind(&patch.predicate)
        .bind(patch.is_active)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(rule_from_row))
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        let res = sqlx::query("DELETE FROM rules WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn count_for_class(&self, class_id: &str) -> Result<i64> {
        check_id(class_id)?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM rules WHERE class_id = $1::uuid")
            .bind(class_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(count)
    }
}
