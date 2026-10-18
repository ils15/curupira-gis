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

/// Rulesets sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgRuleSetRepo {
    pool: PgPool,
}

impl PgRuleSetRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
        let res = sqlx::query("DELETE FROM rulesets WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
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
