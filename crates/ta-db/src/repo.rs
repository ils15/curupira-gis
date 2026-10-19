//! Repositórios async de projetos, camadas e features.
//!
//! Um único contrato para PostGIS (padrão) e GPKG offline (feature
//! `offline-gpkg`). As implementações Pg respeitam:
//! - **bind-only** nas queries (sem concatenação de SQL com dados) — gate themis;
//! - conversão de geometria **exclusivamente** via [`ta_core::geo`] (ponto
//!   único de conversão do projeto): EWKB hex entra e sai do banco;
//! - **bulk insert** via `COPY ... FROM STDIN` (formato texto), com fallback
//!   batched `INSERT ... SELECT FROM UNNEST` se o COPY falhar;
//! - erros normalizados em [`crate::Error`].

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::postgres::PgPoolCopyExt;
use ta_core::feature::Feature;
use ta_core::geo;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;

use crate::error::check_id;
use crate::spatial::TARGET_SRID;
use crate::{Error, Result};

/// Repositório de projetos.
#[async_trait]
pub trait ProjectRepo: Send + Sync {
    /// Insere o projeto; id vazio deixa o banco gerar UUID.
    /// Devolve o projeto persistido (id e created_at preenchidos).
    async fn create(&self, project: &Project) -> Result<Project>;
    async fn get(&self, id: &str) -> Result<Option<Project>>;
    async fn list(&self) -> Result<Vec<Project>>;
    /// Remove o projeto (e camadas/features por CASCADE). `Ok(false)` = ausente.
    async fn delete(&self, id: &str) -> Result<bool>;
    /// Renomeia o projeto. `Ok(false)` = ausente (nome duplicado vira erro
    /// do UNIQUE — normalizado em `Error::Sql`).
    async fn rename(&self, id: &str, new_name: &str) -> Result<bool>;
}

/// Repositório de camadas.
#[async_trait]
pub trait LayerRepo: Send + Sync {
    /// Insere a camada (bbox fica NULL até o primeiro `update_bbox`/`set_bbox`).
    async fn add(&self, layer: &Layer) -> Result<Layer>;
    /// Busca uma camada por id (`None` = ausente).
    async fn get(&self, id: &str) -> Result<Option<Layer>>;
    async fn list_for_project(&self, project_id: &str) -> Result<Vec<Layer>>;
    /// Recalcula e grava o bbox da camada a partir das features
    /// (`ST_Extent` + GIST em `features.geom`). `Ok(false)` = camada ausente.
    async fn update_bbox(&self, layer_id: &str) -> Result<bool>;
    /// Grava o bbox **explícito** da camada (usado por camadas raster, cujas
    /// feições não estão no banco — envelope derivado do geotransform).
    /// `Ok(false)` = camada ausente.
    async fn set_bbox(
        &self,
        layer_id: &str,
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
    ) -> Result<bool>;
    async fn delete(&self, id: &str) -> Result<bool>;
    /// Renomeia a camada. `Ok(false)` = ausente (conflito com o UNIQUE
    /// `(project_id, name)` vira `Error::Sql`).
    async fn rename(&self, id: &str, new_name: &str) -> Result<bool>;
}

/// Repositório de features (geometrias + atributos).
#[async_trait]
pub trait FeatureRepo: Send + Sync {
    /// Inserção em lote; em PostGIS usa COPY FROM STDIN com fallback batched
    /// INSERT (meta: 10k features). Devolve o número de linhas inseridas.
    async fn bulk_insert(&self, layer_id: &str, features: Vec<Feature>) -> Result<u64>;
    /// Todas as features da camada, em ordem de inserção (export/derive).
    async fn list(&self, layer_id: &str) -> Result<Vec<Feature>>;
    /// Features cujo bbox intersecta o envelope dado (operador `&&`, GIST).
    async fn bbox_query(
        &self,
        layer_id: &str,
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
    ) -> Result<Vec<Feature>>;
    async fn count(&self, layer_id: &str) -> Result<u64>;
    /// Features da camada **com o id de linha do banco** (mesma ordem de
    /// `list`). A GUI usa o id para `update_attrs`/`delete_feature`.
    async fn list_with_ids(&self, layer_id: &str) -> Result<Vec<(i64, Feature)>>;
    /// Substitui os atributos JSONB da feature (`attrs` inteiro, não merge).
    /// `Ok(false)` = feature ausente. Bind-only (Fase 3 — edição pela GUI).
    async fn update_attrs(&self, feature_id: i64, attrs: &serde_json::Value) -> Result<bool>;
    /// Remove a feature pelo id de linha. `Ok(false)` = ausente. Bind-only.
    async fn delete_feature(&self, feature_id: i64) -> Result<bool>;
}

// ---------------------------------------------------------------------------
// Implementações Pg
// ---------------------------------------------------------------------------

/// Repositório de projetos sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgProjectRepo {
    pool: PgPool,
}

impl PgProjectRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `create` sobre o **executor** dado (pool ou transação) — o import do
    /// exchange cria o projeto DENTRO da transação única do import (atômico:
    /// falha em qualquer seção não deixa projeto parcial).
    pub async fn create_on<'e, E>(exec: E, project: &Project) -> Result<Project>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        if project.srid != TARGET_SRID {
            return Err(ta_core::Error::SridMismatch {
                expected: TARGET_SRID,
                got: project.srid,
            }
            .into());
        }
        let row: ProjectRow = sqlx::query_as(
            // id vazio → NULL → gen_random_uuid() do banco (PG16 nativo).
            "INSERT INTO projects (id, name, srid) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2, $3) \
             RETURNING id::text, name, srid, created_at",
        )
        .bind(&project.id)
        .bind(&project.name)
        .bind(project.srid as i32)
        .fetch_one(exec)
        .await?;
        project_from_row(row)
    }

    /// Cria o projeto e grava as configurações do fluxo (`active`,
    /// `min_scale`, `max_scale`, `max_cells`, `description`) na MESMA
    /// transação — caminho de quem cria projeto com settings de uma vez.
    pub async fn create_with_flow_settings(
        &self,
        project: &Project,
        settings: &ProjectFlowSettings,
    ) -> Result<Project> {
        let mut tx = self.pool.begin().await?;
        let created = Self::create_on(&mut *tx, project).await?;
        Self::apply_flow_settings_on(&mut *tx, &created.id, settings).await?;
        tx.commit().await?;
        Ok(created)
    }

    /// Contagens do projeto em cada tabela do fluxo — o resumo do que seria
    /// apagado (`project delete` sem `--yes`) e a base do relatório com
    /// `--yes`. `Ok(None)` = projeto ausente. Bind-only; só conta.
    pub async fn delete_preview(&self, id: &str) -> Result<Option<ProjectDeleteReport>> {
        check_id(id)?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE id = $1::uuid)")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;
        if !exists {
            return Ok(None);
        }
        let mut report = ProjectDeleteReport::default();
        {
            let mut tx = self.pool.begin().await?;
            fill_delete_counts(&mut tx, id, &mut report).await?;
            tx.commit().await?;
        }
        Ok(Some(report))
    }

    /// Deleção ORDENADA do projeto em UMA transação — respeita as FK
    /// `ON DELETE RESTRICT` (`rules.class_id`, `draft_cells.class_id`,
    /// `draft_cells.ruleset_id`) SEM desativar triggers: as linhas
    /// dependentes saem ANTES das referenciadas, e o CASCADE do banco segue
    /// valendo como rede de segurança. Devolve o relatório de contagens por
    /// tabela; `Ok(None)` = projeto ausente. Cenas (`scenes`) são globais e
    /// NÃO são apagadas — só o vínculo (`project_scenes`).
    pub async fn delete_ordered(&self, id: &str) -> Result<Option<ProjectDeleteReport>> {
        check_id(id)?;
        let mut tx = self.pool.begin().await?;
        let mut report = ProjectDeleteReport::default();
        fill_delete_counts(&mut tx, id, &mut report).await?;
        // Ordem das deleções (dependências primeiro):
        // decisões → regras → rulesets → log/processos/tasks → medidas →
        // bloqueios → rascunhos → células → grades → features/camadas → AOI →
        // vínculos de cena → classes → grupos → fases → projeto.
        report.draft_cells = sqlx::query(
            "DELETE FROM draft_cells dc USING draft_layers dl \
             WHERE dc.draft_layer_id = dl.id AND dl.project_id = $1::uuid",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.rules = sqlx::query(
            "DELETE FROM rules r USING rulesets rs \
             WHERE r.ruleset_id = rs.id AND rs.project_id = $1::uuid",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.rulesets = sqlx::query("DELETE FROM rulesets WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        report.cell_log = sqlx::query(
            "DELETE FROM cell_log cl USING tasks t \
             WHERE cl.task_id = t.id AND t.project_id = $1::uuid",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.processes = sqlx::query(
            "DELETE FROM processes pr USING tasks t \
             WHERE pr.task_id = t.id AND t.project_id = $1::uuid",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.tasks = sqlx::query("DELETE FROM tasks WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        report.cell_measures = sqlx::query(
            "DELETE FROM cell_measures m USING cell_layers cl \
             WHERE m.cell_layer_id = cl.id AND cl.project_id = $1::uuid",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.blocked_cells = sqlx::query(
            "DELETE FROM blocked_cells b USING cell_layers cl \
             WHERE b.cell_layer_id = cl.id AND cl.project_id = $1::uuid",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.cells = sqlx::query(
            "DELETE FROM cells c USING cell_layers cl \
             WHERE c.cell_layer_id = cl.id AND cl.project_id = $1::uuid",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        // draft_layers ANTES de cell_layers: `draft_layers.cell_layer_id` é
        // CASCADE — se a grade sair primeiro, o rascunho é apagado junto e o
        // DELETE explícito abaixo contaria 0 (o relatório do delete ficaria 1
        // abaixo do preview).
        report.draft_layers = sqlx::query("DELETE FROM draft_layers WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        report.cell_layers = sqlx::query("DELETE FROM cell_layers WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        // aoi_layers ANTES de layers: `aoi_layers.layer_id` é CASCADE — se a
        // camada sair primeiro, o vínculo é apagado junto e o DELETE explícito
        // abaixo contaria 0 (o relatório precisa atribuir cada linha à tabela
        // certa).
        report.aoi_layers = sqlx::query("DELETE FROM aoi_layers WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        report.features = sqlx::query(
            "DELETE FROM features f USING layers l \
             WHERE f.layer_id = l.id AND l.project_id = $1::uuid",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.layers = sqlx::query("DELETE FROM layers WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        // Cenas são globais: sai só o VÍNCULO do projeto com elas.
        report.project_scenes =
            sqlx::query("DELETE FROM project_scenes WHERE project_id = $1::uuid")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        // `classes.parent_id`/`class_group_id` são SET NULL: apagar todas as
        // classes do projeto num único statement não desafia a FK.
        report.classes = sqlx::query("DELETE FROM classes WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        report.class_groups = sqlx::query("DELETE FROM class_groups WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        report.phases = sqlx::query("DELETE FROM phases WHERE project_id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let deleted = sqlx::query("DELETE FROM projects WHERE id = $1::uuid")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted == 0 {
            // Projeto sumiu na corrida: nada a reportar, nada foi apagado.
            tx.rollback().await?;
            return Ok(None);
        }
        tx.commit().await?;
        Ok(Some(report))
    }

    /// UPDATE das configurações do fluxo sobre o **executor** dado.
    pub async fn apply_flow_settings_on<'e, E>(
        exec: E,
        project_id: &str,
        settings: &ProjectFlowSettings,
    ) -> Result<bool>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        check_id(project_id)?;
        let res = sqlx::query(
            "UPDATE projects SET active = $2, min_scale = $3, max_scale = $4, \
                    max_cells = $5, description = $6, updated_on = now() \
             WHERE id = $1::uuid",
        )
        .bind(project_id)
        .bind(settings.active)
        .bind(settings.min_scale)
        .bind(settings.max_scale)
        .bind(settings.max_cells)
        .bind(&settings.description)
        .execute(exec)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Lê as configurações do fluxo do projeto (`None` = projeto ausente) —
    /// seção `project` do export do exchange.
    pub async fn flow_settings(&self, project_id: &str) -> Result<Option<ProjectFlowSettings>> {
        check_id(project_id)?;
        let row: Option<FlowSettingsRow> = sqlx::query_as(
            "SELECT active, min_scale, max_scale, max_cells, description \
             FROM projects WHERE id = $1::uuid",
        )
        .bind(project_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(active, min_scale, max_scale, max_cells, description)| ProjectFlowSettings {
                active,
                min_scale,
                max_scale,
                max_cells,
                description,
            },
        ))
    }
}

/// Configurações do fluxo de um projeto (`projects`, colunas da `0005` e
/// `description` da `0014`) — a seção `project` do exchange.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectFlowSettings {
    /// Projeto ativo.
    pub active: bool,
    /// Escala mínima de edição (opcional).
    pub min_scale: Option<f64>,
    /// Escala máxima de edição (opcional).
    pub max_scale: Option<f64>,
    /// Teto de células por grade.
    pub max_cells: i32,
    /// Descrição livre.
    pub description: String,
}

/// Relatório da deleção ordenada de um projeto — contagens por tabela
/// (o `project delete --yes` imprime; o preview sem `--yes` mostra o mesmo
/// resumo antes de decidir). Cenas globais não são apagadas, então não têm
/// contagem aqui — só o vínculo (`project_scenes`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectDeleteReport {
    /// Decisões de classificação apagadas.
    pub draft_cells: u64,
    /// Regras apagadas.
    pub rules: u64,
    /// Rulesets apagados.
    pub rulesets: u64,
    /// Entradas de log por célula apagadas.
    pub cell_log: u64,
    /// Processos (por célula) apagados.
    pub processes: u64,
    /// Tasks da fila apagadas.
    pub tasks: u64,
    /// Medidas por célula apagadas.
    pub cell_measures: u64,
    /// Células bloqueadas apagadas.
    pub blocked_cells: u64,
    /// Células das grades apagadas.
    pub cells: u64,
    /// Grades de células apagadas.
    pub cell_layers: u64,
    /// Rascunhos apagados.
    pub draft_layers: u64,
    /// Feições de camadas do projeto apagadas.
    pub features: u64,
    /// Camadas vetoriais apagadas.
    pub layers: u64,
    /// Vínculos de AOI apagados.
    pub aoi_layers: u64,
    /// Vínculos projeto↔cena apagados (as cenas continuam).
    pub project_scenes: u64,
    /// Classes apagadas.
    pub classes: u64,
    /// Grupos de classes apagados.
    pub class_groups: u64,
    /// Fases apagadas.
    pub phases: u64,
}

impl ProjectDeleteReport {
    /// Total de linhas apagadas em todas as tabelas do projeto.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.draft_cells
            + self.rules
            + self.rulesets
            + self.cell_log
            + self.processes
            + self.tasks
            + self.cell_measures
            + self.blocked_cells
            + self.cells
            + self.cell_layers
            + self.draft_layers
            + self.features
            + self.layers
            + self.aoi_layers
            + self.project_scenes
            + self.classes
            + self.class_groups
            + self.phases
    }

    /// Pares `(rótulo, contagem)` na ordem da deleção — o relatório impresso.
    #[must_use]
    pub fn entries(&self) -> [(&'static str, u64); 18] {
        [
            ("draft_cells", self.draft_cells),
            ("rules", self.rules),
            ("rulesets", self.rulesets),
            ("cell_log", self.cell_log),
            ("processes", self.processes),
            ("tasks", self.tasks),
            ("cell_measures", self.cell_measures),
            ("blocked_cells", self.blocked_cells),
            ("cells", self.cells),
            ("cell_layers", self.cell_layers),
            ("draft_layers", self.draft_layers),
            ("features", self.features),
            ("layers", self.layers),
            ("aoi_layers", self.aoi_layers),
            ("project_scenes", self.project_scenes),
            ("classes", self.classes),
            ("class_groups", self.class_groups),
            ("phases", self.phases),
        ]
    }
}

/// Preenche [`ProjectDeleteReport`] com as contagens ATUAIS do projeto —
/// SELECT count(*) por tabela (mesma junção das deleções), bind-only, sobre o
/// executor dado (pool, transação do preview ou da própria deleção). Duas
/// consultas porque o `FromRow` de tuplas do sqlx vai até 16 colunas.
async fn fill_delete_counts(
    exec: &mut sqlx::PgConnection,
    project_id: &str,
    report: &mut ProjectDeleteReport,
) -> Result<()> {
    type Parte1 = (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64);
    type Parte2 = (i64, i64, i64, i64, i64, i64, i64, i64);
    let (dc, ru, rs, lg, pr, ta, me, bc, ce, cl): Parte1 = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM draft_cells dc JOIN draft_layers dl \
              ON dc.draft_layer_id = dl.id WHERE dl.project_id = $1::uuid), \
           (SELECT count(*) FROM rules r JOIN rulesets rs ON r.ruleset_id = rs.id \
              WHERE rs.project_id = $1::uuid), \
           (SELECT count(*) FROM rulesets WHERE project_id = $1::uuid), \
           (SELECT count(*) FROM cell_log lg JOIN tasks t ON lg.task_id = t.id \
              WHERE t.project_id = $1::uuid), \
           (SELECT count(*) FROM processes pr JOIN tasks t ON pr.task_id = t.id \
              WHERE t.project_id = $1::uuid), \
           (SELECT count(*) FROM tasks WHERE project_id = $1::uuid), \
           (SELECT count(*) FROM cell_measures m JOIN cell_layers cl \
              ON m.cell_layer_id = cl.id WHERE cl.project_id = $1::uuid), \
           (SELECT count(*) FROM blocked_cells b JOIN cell_layers cl \
              ON b.cell_layer_id = cl.id WHERE cl.project_id = $1::uuid), \
           (SELECT count(*) FROM cells c JOIN cell_layers cl \
              ON c.cell_layer_id = cl.id WHERE cl.project_id = $1::uuid), \
           (SELECT count(*) FROM cell_layers WHERE project_id = $1::uuid)",
    )
    .bind(project_id)
    .fetch_one(&mut *exec)
    .await?;
    let (dl, fe, la, ao, ps, cl2, cg, ph): Parte2 = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM draft_layers WHERE project_id = $1::uuid), \
           (SELECT count(*) FROM features f JOIN layers l ON f.layer_id = l.id \
              WHERE l.project_id = $1::uuid), \
           (SELECT count(*) FROM layers WHERE project_id = $1::uuid), \
           (SELECT count(*) FROM aoi_layers WHERE project_id = $1::uuid), \
           (SELECT count(*) FROM project_scenes WHERE project_id = $1::uuid), \
           (SELECT count(*) FROM classes WHERE project_id = $1::uuid), \
           (SELECT count(*) FROM class_groups WHERE project_id = $1::uuid), \
           (SELECT count(*) FROM phases WHERE project_id = $1::uuid)",
    )
    .bind(project_id)
    .fetch_one(&mut *exec)
    .await?;
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    report.draft_cells = n(dc);
    report.rules = n(ru);
    report.rulesets = n(rs);
    report.cell_log = n(lg);
    report.processes = n(pr);
    report.tasks = n(ta);
    report.cell_measures = n(me);
    report.blocked_cells = n(bc);
    report.cells = n(ce);
    report.cell_layers = n(cl);
    report.draft_layers = n(dl);
    report.features = n(fe);
    report.layers = n(la);
    report.aoi_layers = n(ao);
    report.project_scenes = n(ps);
    report.classes = n(cl2);
    report.class_groups = n(cg);
    report.phases = n(ph);
    Ok(())
}

/// Linha de `flow_settings` — `(active, min_scale, max_scale, max_cells, description)`.
type FlowSettingsRow = (bool, Option<f64>, Option<f64>, i32, String);

type ProjectRow = (String, String, i32, time::OffsetDateTime);

fn project_from_row(row: ProjectRow) -> Result<Project> {
    Ok(Project {
        id: row.0,
        name: row.1,
        srid: u32::try_from(row.2).map_err(|_| Error::InvalidId(row.2.to_string()))?,
        created_at: row.3,
    })
}

#[async_trait]
impl ProjectRepo for PgProjectRepo {
    async fn create(&self, project: &Project) -> Result<Project> {
        Self::create_on(&self.pool, project).await
    }

    async fn get(&self, id: &str) -> Result<Option<Project>> {
        check_id(id)?;
        let row: Option<ProjectRow> = sqlx::query_as(
            "SELECT id::text, name, srid, created_at FROM projects WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(project_from_row).transpose()
    }

    async fn list(&self) -> Result<Vec<Project>> {
        let rows: Vec<ProjectRow> =
            sqlx::query_as("SELECT id::text, name, srid, created_at FROM projects ORDER BY name")
                .fetch_all(&self.pool)
                .await?;
        rows.into_iter().map(project_from_row).collect()
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        let res = sqlx::query("DELETE FROM projects WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<bool> {
        check_id(id)?;
        let res = sqlx::query("UPDATE projects SET name = $2 WHERE id = $1::uuid")
            .bind(id)
            .bind(new_name)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// Repositório de camadas sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgLayerRepo {
    pool: PgPool,
}

impl PgLayerRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// `add` sobre o **executor** dado (pool ou transação) — mesmo SQL do
    /// trait, exposto para `ops.rs` compor camada derivada + features + bbox
    /// em UMA transação (erro SQL nunca deixa camada derivada órfã).
    pub async fn add_on<'e, E>(exec: E, layer: &Layer) -> Result<Layer>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        check_id(&layer.project_id)?;
        if layer.srid != TARGET_SRID {
            return Err(ta_core::Error::SridMismatch {
                expected: TARGET_SRID,
                got: layer.srid,
            }
            .into());
        }
        let gt = geom_type_to_text(layer.geom_type)
            .ok_or(Error::UnsupportedGeomType(layer.geom_type))?;
        let row: LayerRow = sqlx::query_as(
            "INSERT INTO layers (id, project_id, name, geom_type, srid, source_uri) \
             VALUES (COALESCE(NULLIF($1, '')::uuid, gen_random_uuid()), $2::uuid, $3, $4, $5, $6) \
             RETURNING id::text, project_id::text, name, geom_type, srid, source_uri",
        )
        .bind(&layer.id)
        .bind(&layer.project_id)
        .bind(&layer.name)
        .bind(gt)
        .bind(layer.srid as i32)
        .bind(&layer.source_uri)
        .fetch_one(exec)
        .await?;
        layer_from_row(row)
    }

    /// `update_bbox` sobre o **executor** dado (pool ou transação).
    pub async fn update_bbox_on<'e, E>(exec: E, layer_id: &str) -> Result<bool>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        check_id(layer_id)?;
        // 4674 é constante do schema (CHECK features_srid_4674), não dado do
        // usuário — literal em SQL estático, sem formatação.
        let res = sqlx::query(
            "WITH ext AS ( \
                 SELECT ST_SetSRID(ST_Extent(geom)::geometry, 4674) AS b \
                 FROM features WHERE layer_id = $1::uuid \
             ) \
             UPDATE layers SET bbox = ext.b \
             FROM ext \
             WHERE layers.id = $1::uuid AND ext.b IS NOT NULL",
        )
        .bind(layer_id)
        .execute(exec)
        .await?;
        Ok(res.rows_affected() > 0)
    }
}

fn geom_type_to_text(gt: GeomType) -> Option<&'static str> {
    // `geometry` cobre GeometryCollection (tipo genérico do CHECK do schema);
    // raster é camada de referência (source_uri), migrado em 0004.
    match gt {
        GeomType::Point => Some("point"),
        GeomType::LineString => Some("linestring"),
        GeomType::Polygon => Some("polygon"),
        GeomType::MultiPoint => Some("multipoint"),
        GeomType::MultiLineString => Some("multilinestring"),
        GeomType::MultiPolygon => Some("multipolygon"),
        GeomType::GeometryCollection => Some("geometry"),
        GeomType::Raster => Some("raster"),
    }
}

fn geom_type_from_text(text: &str) -> Option<GeomType> {
    match text {
        "point" => Some(GeomType::Point),
        "linestring" => Some(GeomType::LineString),
        "polygon" => Some(GeomType::Polygon),
        "multipoint" => Some(GeomType::MultiPoint),
        "multilinestring" => Some(GeomType::MultiLineString),
        "multipolygon" => Some(GeomType::MultiPolygon),
        "geometry" => Some(GeomType::GeometryCollection),
        "raster" => Some(GeomType::Raster),
        _ => None,
    }
}

type LayerRow = (
    String,
    String,
    String,
    String,
    i32,
    Option<String>, // source_uri
);

fn layer_from_row(row: LayerRow) -> Result<Layer> {
    Ok(Layer {
        id: row.0,
        project_id: row.1,
        name: row.2,
        geom_type: geom_type_from_text(&row.3)
            .ok_or(Error::UnsupportedGeomType(GeomType::Raster))?,
        srid: u32::try_from(row.4).map_err(|_| Error::InvalidId(row.4.to_string()))?,
        source_uri: row.5.unwrap_or_default(),
    })
}

#[async_trait]
impl LayerRepo for PgLayerRepo {
    async fn add(&self, layer: &Layer) -> Result<Layer> {
        Self::add_on(&self.pool, layer).await
    }

    async fn get(&self, id: &str) -> Result<Option<Layer>> {
        check_id(id)?;
        let row: Option<LayerRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, geom_type, srid, source_uri \
             FROM layers WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(layer_from_row).transpose()
    }

    async fn list_for_project(&self, project_id: &str) -> Result<Vec<Layer>> {
        check_id(project_id)?;
        let rows: Vec<LayerRow> = sqlx::query_as(
            "SELECT id::text, project_id::text, name, geom_type, srid, source_uri \
             FROM layers WHERE project_id = $1::uuid ORDER BY name",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(layer_from_row).collect()
    }

    async fn update_bbox(&self, layer_id: &str) -> Result<bool> {
        Self::update_bbox_on(&self.pool, layer_id).await
    }

    async fn set_bbox(
        &self,
        layer_id: &str,
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
    ) -> Result<bool> {
        check_id(layer_id)?;
        // 4674 é SRID canônico do schema (CONSTRAINT), constante estática.
        let res = sqlx::query(
            "UPDATE layers \
             SET bbox = ST_MakeEnvelope($2, $3, $4, $5, 4674) \
             WHERE id = $1::uuid",
        )
        .bind(layer_id)
        .bind(min_x)
        .bind(min_y)
        .bind(max_x)
        .bind(max_y)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        check_id(id)?;
        let res = sqlx::query("DELETE FROM layers WHERE id = $1::uuid")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<bool> {
        check_id(id)?;
        let res = sqlx::query("UPDATE layers SET name = $2 WHERE id = $1::uuid")
            .bind(id)
            .bind(new_name)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// Repositório de features sobre [`PgPool`].
#[derive(Debug, Clone)]
pub struct PgFeatureRepo {
    pool: PgPool,
}

impl PgFeatureRepo {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// INSERT em lote (UNNEST, bind-only) sobre a **conexão** dada (de um pool
    /// ou transação) — caminho do import do exchange, que grava
    /// camada+features numa única transação (o COPY do `bulk_insert` é
    /// pool-bound; aqui volumes são pequenos).
    pub async fn insert_on(
        exec: &mut sqlx::PgConnection,
        layer_id: &str,
        features: &[Feature],
    ) -> Result<u64> {
        const CHUNK: usize = 1000;
        let mut total = 0u64;
        for chunk in features.chunks(CHUNK) {
            let ewkb_hex: Vec<String> = chunk
                .iter()
                .map(|f| {
                    geo::to_ewkb(&f.geom, TARGET_SRID)
                        .map(|ewkb| crate::spatial::bytes_to_hex(&ewkb))
                })
                .collect::<ta_core::Result<Vec<String>>>()?;
            let attrs: Vec<serde_json::Value> = chunk.iter().map(|f| f.attrs.clone()).collect();
            let res = sqlx::query(
                "INSERT INTO features (layer_id, geom, attrs) \
                 SELECT $1::uuid, x.g::geometry, x.a \
                 FROM UNNEST($2::text[], $3::jsonb[]) AS x(g, a)",
            )
            .bind(layer_id)
            .bind(&ewkb_hex)
            .bind(&attrs)
            .execute(&mut *exec)
            .await?;
            total += res.rows_affected();
        }
        Ok(total)
    }

    /// COPY FROM STDIN (formato texto): `layer_id \t ewkb_hex \t json`.
    /// Hex-EWKB contém só [0-9a-f], sem necessidade de escape; o JSON de
    /// atributos passa por [`escape_copy_text`].
    async fn copy_insert(&self, payload: &str) -> sqlx::Result<u64> {
        let mut copy = self
            .pool
            .copy_in_raw("COPY features (layer_id, geom, attrs) FROM STDIN WITH (FORMAT text)")
            .await?;
        if let Err(e) = copy.send(payload.as_bytes()).await {
            // Devolve a conexão ao pool em estado utilizável antes de propagar.
            let _ = copy.abort("falha ao enviar payload do COPY").await;
            return Err(e);
        }
        copy.finish().await
    }

    /// Fallback: batched INSERT bind-only via UNNEST (1000 linhas por lote).
    async fn insert_fallback(
        &self,
        layer_id: &str,
        features: &[Feature],
        ewkb_hex: &[String],
    ) -> Result<u64> {
        const CHUNK: usize = 1000;
        let mut tx = self.pool.begin().await?;
        let mut total = 0u64;
        for (i, chunk) in features.chunks(CHUNK).enumerate() {
            let hex_chunk = &ewkb_hex[i * CHUNK..(i * CHUNK) + chunk.len()];
            let attrs: Vec<serde_json::Value> = chunk.iter().map(|f| f.attrs.clone()).collect();
            let res = sqlx::query(
                "INSERT INTO features (layer_id, geom, attrs) \
                 SELECT $1::uuid, x.g::geometry, x.a \
                 FROM UNNEST($2::text[], $3::jsonb[]) AS x(g, a)",
            )
            .bind(layer_id)
            .bind(hex_chunk)
            .bind(&attrs)
            .execute(&mut *tx)
            .await?;
            total += res.rows_affected();
        }
        tx.commit().await?;
        Ok(total)
    }
}

#[async_trait]
impl FeatureRepo for PgFeatureRepo {
    async fn bulk_insert(&self, layer_id: &str, features: Vec<Feature>) -> Result<u64> {
        if features.is_empty() {
            return Ok(0);
        }
        check_id(layer_id)?;

        let mut payload = String::with_capacity(features.len() * 96);
        let mut ewkb_hex = Vec::with_capacity(features.len());
        for feature in &features {
            let ewkb = geo::to_ewkb(&feature.geom, TARGET_SRID)?;
            let hex = bytes_to_hex(&ewkb);
            ewkb_hex.push(hex.clone());
            payload.push_str(layer_id);
            payload.push('\t');
            payload.push_str(&hex);
            payload.push('\t');
            let attrs = serde_json::to_string(&feature.attrs).map_err(ta_core::Error::from)?;
            escape_copy_text(&attrs, &mut payload);
            payload.push('\n');
        }

        match self.copy_insert(&payload).await {
            Ok(rows) => Ok(rows),
            Err(copy_err) => match self.insert_fallback(layer_id, &features, &ewkb_hex).await {
                Ok(rows) => Ok(rows),
                Err(fallback_err) => Err(Error::CopyAndInsertFailed {
                    copy: copy_err.to_string(),
                    source: Box::new(fallback_err),
                }),
            },
        }
    }

    async fn list(&self, layer_id: &str) -> Result<Vec<Feature>> {
        check_id(layer_id)?;
        let rows: Vec<(Vec<u8>, serde_json::Value)> = sqlx::query_as(
            "SELECT ST_AsEWKB(geom), attrs FROM features \
             WHERE layer_id = $1::uuid ORDER BY id",
        )
        .bind(layer_id)
        .fetch_all(&self.pool)
        .await?;
        rows_into_features(rows)
    }

    async fn bbox_query(
        &self,
        layer_id: &str,
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
    ) -> Result<Vec<Feature>> {
        check_id(layer_id)?;
        let rows: Vec<(Vec<u8>, serde_json::Value)> = sqlx::query_as(
            "SELECT ST_AsEWKB(geom), attrs FROM features \
             WHERE layer_id = $1::uuid \
               AND geom && ST_MakeEnvelope($2, $3, $4, $5, 4674)",
        )
        .bind(layer_id)
        .bind(min_x)
        .bind(min_y)
        .bind(max_x)
        .bind(max_y)
        .fetch_all(&self.pool)
        .await?;
        rows_into_features(rows)
    }

    async fn count(&self, layer_id: &str) -> Result<u64> {
        check_id(layer_id)?;
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM features WHERE layer_id = $1::uuid")
            .bind(layer_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    async fn list_with_ids(&self, layer_id: &str) -> Result<Vec<(i64, Feature)>> {
        check_id(layer_id)?;
        // id + geometria + attrs numa única query ordenada por id: a tupla
        // (id, feature) nunca desalinha (sem zip de queries separadas).
        let rows: Vec<(i64, Vec<u8>, serde_json::Value)> = sqlx::query_as(
            "SELECT id, ST_AsEWKB(geom), attrs FROM features \
             WHERE layer_id = $1::uuid ORDER BY id",
        )
        .bind(layer_id)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, ewkb, attrs) in rows {
            let (geom, srid) = geo::from_ewkb(&ewkb)?;
            if let Some(srid) = srid {
                if srid != TARGET_SRID {
                    return Err(ta_core::Error::SridMismatch {
                        expected: TARGET_SRID,
                        got: srid,
                    }
                    .into());
                }
            }
            out.push((id, Feature { geom, attrs }));
        }
        Ok(out)
    }

    async fn update_attrs(&self, feature_id: i64, attrs: &serde_json::Value) -> Result<bool> {
        let res = sqlx::query("UPDATE features SET attrs = $2 WHERE id = $1")
            .bind(feature_id)
            .bind(attrs)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn delete_feature(&self, feature_id: i64) -> Result<bool> {
        let res = sqlx::query("DELETE FROM features WHERE id = $1")
            .bind(feature_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// EWKB + attrs → features, com defesa em profundidade de SRID (o typmod
/// garante 4674; se um dia divergir, falha explícita).
fn rows_into_features(rows: Vec<(Vec<u8>, serde_json::Value)>) -> Result<Vec<Feature>> {
    let mut out = Vec::with_capacity(rows.len());
    for (ewkb, attrs) in rows {
        let (geom, srid) = geo::from_ewkb(&ewkb)?;
        if let Some(srid) = srid {
            if srid != TARGET_SRID {
                return Err(ta_core::Error::SridMismatch {
                    expected: TARGET_SRID,
                    got: srid,
                }
                .into());
            }
        }
        out.push(Feature { geom, attrs });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helpers de encoding (COPY texto)
// ---------------------------------------------------------------------------

/// Escapa um valor para o formato texto do COPY (`\` → `\\`, tab → `\t`,
/// LF → `\n`, CR → `\r`). NUL (0x00) não é representável — é descartado,
/// como faz o ecossistema PostgreSQL para texto.
fn escape_copy_text(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => {}
            other => out.push(other),
        }
    }
}

/// Hex minúsculo sem dependência externa (EWKB textual do PostGIS).
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mapeamento_geom_type_bidirecional() {
        for gt in [
            GeomType::Point,
            GeomType::LineString,
            GeomType::Polygon,
            GeomType::MultiPoint,
            GeomType::MultiLineString,
            GeomType::MultiPolygon,
            GeomType::GeometryCollection,
            GeomType::Raster,
        ] {
            let text = geom_type_to_text(gt).expect("todo GeomType deve mapear");
            assert_eq!(geom_type_from_text(text), Some(gt));
        }
        // texto de collection volta como GeometryCollection
        assert_eq!(
            geom_type_from_text("geometry"),
            Some(GeomType::GeometryCollection)
        );
        assert_eq!(geom_type_from_text("desconhecido"), None);
    }

    #[test]
    fn escape_copy_cobre_separadores_e_escape() {
        let mut out = String::new();
        escape_copy_text("a\tb\nc\rd\\e", &mut out);
        assert_eq!(out, "a\\tb\\nc\\rd\\\\e");

        // JSON com aspas/backslash não quebra a linha do COPY.
        let mut out2 = String::new();
        escape_copy_text(&json!({"nome": "ta\nba", "v": 1}).to_string(), &mut out2);
        assert!(!out2.contains('\t') && !out2.contains('\n'));
    }

    #[test]
    fn hex_bate_com_formato_do_postgis() {
        assert_eq!(bytes_to_hex(&[0x01, 0xff, 0x00]), "01ff00");
        assert_eq!(bytes_to_hex(&[]), "");
    }
}
