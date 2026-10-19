//! Subcomandos do **fluxo de monitoramento** (Fase 1): fase, AOI, grade de
//! células e células bloqueadas.
//!
//! O cálculo da grade é **delegado ao motor puro** `ta-engine` (sem IO/DB); aqui
//! ficam só a leitura/gravação no banco (via `ta-db`, bind-only) e a costura
//! transacional. Erros chegam à CLI como texto acionável.

use geo_types::{Geometry, MultiPolygon, Polygon};
use sqlx::PgPool;
use ta_core::aoi::AoiLayer;
use ta_core::cell::{BlockedCell, Cell, CellLayer, GridSpec};
use ta_core::classification::{Class, ClassGroup, DraftLayer};
use ta_core::feature::Feature;
use ta_core::phase::Phase;
use ta_core::rule::{Rule, RuleSet};
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{
    BlockedCellRepo, CellLayerRepo, CellRepo, PgBlockedCellRepo, PgCellLayerRepo, PgCellRepo,
};
use ta_db::repo_class::{
    ClassGroupRepo, ClassRepo, DraftLayerRepo, PgClassGroupRepo, PgClassRepo, PgDraftLayerRepo,
};
use ta_db::repo_phase::{PgPhaseRepo, PhaseRepo};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RulePatch, RuleRepo, RuleSetRepo};
use ta_db::spatial::{TARGET_SRID, srid_units, transform_geometry};
use ta_engine::{aoi_bbox, candidate_count, evaluate, generate_grid, parse_predicate};

use crate::resolve_project;

/// `phase create` — cria uma fase no projeto.
pub async fn phase_create(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    description: Option<String>,
    sort_order: i32,
    cell_blocker: bool,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let mut phase = Phase::new(&project.id, name);
    phase.description = description.unwrap_or_default();
    phase.sort_order = sort_order;
    phase.cell_blocker = cell_blocker;

    let saved = PgPhaseRepo::new(pool.clone())
        .create(&phase)
        .await
        .map_err(|e| phase_create_error(&e, name))?;
    println!(
        "fase criada: {}\tprojeto={}\tbloqueia_celulas={}",
        saved.id, project.name, saved.cell_blocker
    );
    pool.close().await;
    Ok(())
}

/// `phase list` — lista as fases do projeto (ordem de exibição).
pub async fn phase_list(dsn: Option<&str>, project: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let phases = PgPhaseRepo::new(pool.clone())
        .list_for_project(&project.id)
        .await
        .map_err(|e| e.to_string())?;
    if phases.is_empty() {
        println!("(nenhuma fase no projeto `{}`)", project.name);
    }
    for p in &phases {
        println!(
            "{}\tsort={}\tbloqueia_celulas={}\t{}",
            p.id, p.sort_order, p.cell_blocker, p.name
        );
    }
    pool.close().await;
    Ok(())
}

/// `aoi create` — vincula uma camada existente como AOI do projeto.
///
/// `phase` (opcional) associa a AOI a uma fase do MESMO projeto (validado).
pub async fn aoi_create(
    dsn: Option<&str>,
    project: &str,
    layer: &str,
    id_column: Option<String>,
    phase: Option<&str>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let layer_row = PgLayerRepo::new(pool.clone())
        .get(layer)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("camada `{layer}` não existe"))?;
    if layer_row.project_id != project.id {
        return Err(format!(
            "camada `{layer}` pertence a outro projeto ({})",
            layer_row.project_id
        ));
    }

    // Fase opcional: precisa existir e ser do MESMO projeto (erro acionável).
    let phase_id = match phase {
        Some(pid) => Some(ensure_phase_in_project(&pool, &project.id, pid).await?),
        None => None,
    };

    let saved = PgAoiLayerRepo::new(pool.clone())
        .create(
            &AoiLayer::new(&project.id, &layer_row.id, id_column).with_phase(phase_id.as_deref()),
        )
        .await
        .map_err(|e| format!("criar AOI: {e}"))?;
    println!(
        "aoi criada: {}\tprojeto={}\tcamada={} ({})",
        saved.id, project.name, layer_row.name, layer_row.id
    );
    pool.close().await;
    Ok(())
}

/// `grid create` — gera a grade com o motor puro e persiste camada + células
/// em **uma transação**.
///
/// `resolution` e a origem são expressas **nas unidades do SRS `srid`** (graus
/// se geográfico, ex.: 4674; metros se projetado, ex.: 5880) — Decisão 4
/// retificada em 2026-10-18. A AOI é lida em 4674 e reprojetada para `srid`
/// (PostGIS `ST_Transform`) antes do recorte; as células geradas em `srid` são
/// persistidas em `cells.geom` (canônico, EPSG:4674).
///
/// `phase` (opcional) associa a grade a uma fase do MESMO projeto (validado).
// Assinatura plana da CLI (8 parâmetros) refletindo as flags de `grid create`;
// agrupar num struct só para calar o lint adicionaria ruído a um único chamador.
#[allow(clippy::too_many_arguments)]
pub async fn grid_create(
    dsn: Option<&str>,
    aoi_layer_id: &str,
    resolution: f64,
    origin_x: Option<f64>,
    origin_y: Option<f64>,
    srid: i32,
    name: Option<String>,
    phase: Option<&str>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    // EPSG precisa existir em `spatial_ref_sys` (erro acionável antes de
    // qualquer reprojeção) e define as unidades da resolução/origem.
    let units = srid_units(&pool, srid).await.map_err(|e| e.to_string())?;
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .get(aoi_layer_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("AOI `{aoi_layer_id}` não existe"))?;

    // Geometria da AOI = todas as feições poligonais da camada vinculada,
    // lidas no SRS de armazenamento (4674).
    let features = PgFeatureRepo::new(pool.clone())
        .list(&aoi.layer_id)
        .await
        .map_err(|e| format!("lendo a geometria da AOI: {e}"))?;
    let area_storage = multipolygon_from_features(&features)?;
    // Recorte acontece no SRS da grade; a AOI é reprojetada 4674 → srid.
    let area = aoi_in_srid(&pool, &area_storage, srid).await?;

    let origin_x = origin_x.unwrap_or_else(|| aoi_bbox(&area).map(|b| b[0]).unwrap_or(0.0));
    let origin_y = origin_y.unwrap_or_else(|| aoi_bbox(&area).map(|b| b[1]).unwrap_or(0.0));
    let spec = GridSpec::new(resolution, origin_x, origin_y, srid);

    // Teto de células (`projects.max_cells`): aplicado UMA vez, dentro de
    // `persist_grid` (mesma função usada pelo seed) — não repetir aqui.

    // Fase opcional: precisa existir e ser do MESMO projeto (erro acionável).
    let phase_id = match phase {
        Some(pid) => Some(ensure_phase_in_project(&pool, &aoi.project_id, pid).await?),
        None => None,
    };

    let name = name.unwrap_or_else(|| "grade".to_owned());
    let (layer, inserted) = persist_grid(
        &pool,
        &aoi.project_id,
        &aoi.id,
        &name,
        &spec,
        &area,
        phase_id.as_deref(),
    )
    .await?;
    let count = PgCellRepo::new(pool.clone())
        .count(&layer.id)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "grade criada: {}\tnome={}\tresolucao={} ({})\tsrid={}\n  células: {} \
         (inseridas agora: {inserted})",
        layer.id, layer.name, layer.resolution, units, layer.srid, count
    );
    pool.close().await;
    Ok(())
}

/// `grid info` — contagem e envelope da grade.
pub async fn grid_info(dsn: Option<&str>, cell_layer_id: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let layer = PgCellLayerRepo::new(pool.clone())
        .get(cell_layer_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("camada de células `{cell_layer_id}` não existe"))?;
    let units = srid_units(&pool, layer.srid)
        .await
        .map_err(|e| e.to_string())?;
    let cells = PgCellRepo::new(pool.clone());
    let count = cells.count(&layer.id).await.map_err(|e| e.to_string())?;
    let extent = cells.extent(&layer.id).await.map_err(|e| e.to_string())?;
    let blocked = PgBlockedCellRepo::new(pool.clone())
        .list(&layer.id)
        .await
        .map_err(|e| e.to_string())?
        .len();
    println!(
        "camada: {}\tnome={}\tresolucao={} ({units})\tsrid={}\n  \
         origem=({}, {}) EPSG:{}\n  células: {count}\tbloqueadas: {blocked}",
        layer.id,
        layer.name,
        layer.resolution,
        layer.srid,
        layer.origin_x,
        layer.origin_y,
        layer.srid
    );
    match extent {
        Some([min_x, min_y, max_x, max_y]) => {
            println!(
                "  extensão (EPSG:4674, armazenamento): [{min_x}, {min_y}] – [{max_x}, {max_y}]"
            )
        }
        None => println!("  extensão: (vazia)"),
    }
    pool.close().await;
    Ok(())
}

/// `cell block` — marca uma célula como bloqueada.
pub async fn cell_block(
    dsn: Option<&str>,
    cell_layer_id: &str,
    cell_id: &str,
    reason: Option<String>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let cells = PgCellRepo::new(pool.clone());
    if !cells
        .exists(cell_layer_id, cell_id)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err(format!(
            "célula `{cell_id}` não existe na camada {cell_layer_id} \
             (rode `grid info --cell-layer` para ver a grade)"
        ));
    }
    let saved = PgBlockedCellRepo::new(pool.clone())
        .block(&BlockedCell::new(cell_layer_id, cell_id, reason))
        .await
        .map_err(|e| format!("bloquear célula: {e}"))?;
    println!(
        "célula bloqueada: {}\tcamada={}",
        saved.cell_id, saved.cell_layer_id
    );
    pool.close().await;
    Ok(())
}

/// `cell list-blocked` — lista as células bloqueadas da camada.
pub async fn cell_list_blocked(dsn: Option<&str>, cell_layer_id: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let blocked = PgBlockedCellRepo::new(pool.clone())
        .list(cell_layer_id)
        .await
        .map_err(|e| e.to_string())?;
    if blocked.is_empty() {
        println!("(nenhuma célula bloqueada na camada {cell_layer_id})");
    }
    for b in &blocked {
        println!(
            "{}\t{}",
            b.cell_id,
            b.reason.as_deref().unwrap_or("(sem motivo)")
        );
    }
    pool.close().await;
    Ok(())
}

/// Converte as feições de uma camada em um [`MultiPolygon`] (AOI poligonal),
/// lidas no SRS de armazenamento (EPSG:4674). Feições não poligonais são
/// ignoradas; se não sobrar nenhuma, é erro.
pub fn multipolygon_from_features(features: &[Feature]) -> Result<MultiPolygon<f64>, String> {
    let mut polygons: Vec<Polygon<f64>> = Vec::new();
    for feature in features {
        match &feature.geom {
            Geometry::Polygon(p) => polygons.push(p.clone()),
            Geometry::MultiPolygon(mp) => polygons.extend(mp.0.iter().cloned()),
            _ => {}
        }
    }
    if polygons.is_empty() {
        return Err("a AOI precisa de pelo menos uma feição poligonal".to_owned());
    }
    Ok(MultiPolygon::new(polygons))
}

/// AOI no SRS da grade: reprojetа 4674 → `srid` com PostGIS `ST_Transform`.
///
/// Quando `srid` já é o SRS de armazenamento (4674), devolve a geometria
/// **sem** round-trip (nenhuma transformação é aplicada desnecessariamente).
async fn aoi_in_srid(
    pool: &PgPool,
    area_4674: &MultiPolygon<f64>,
    srid: i32,
) -> Result<MultiPolygon<f64>, String> {
    if srid == TARGET_SRID as i32 {
        return Ok(area_4674.clone());
    }
    let target = u32::try_from(srid).map_err(|_| format!("EPSG inválido: {srid}"))?;
    let geom = transform_geometry(
        pool,
        &Geometry::MultiPolygon(area_4674.clone()),
        TARGET_SRID,
        target,
    )
    .await
    .map_err(|e| format!("reprojetando a AOI para EPSG:{srid}: {e}"))?;
    match geom {
        Geometry::MultiPolygon(mp) => Ok(mp),
        other => Err(format!(
            "reprojeção da AOI produziu geometria inesperada: {other:?}"
        )),
    }
}

/// Gera a grade (motor puro) e persiste camada + células em UMA transação.
///
/// Devolve a camada criada e o número de células efetivamente inseridas. É
/// idempotente no nível das células (`ON CONFLICT DO NOTHING`). Aplica a mesma
/// política de `projects.max_cells` do caminho do CLI (`enforce_max_cells`),
/// para que a regra do projeto seja única.
pub async fn persist_grid(
    pool: &PgPool,
    project_id: &str,
    aoi_layer_id: &str,
    name: &str,
    spec: &GridSpec,
    area: &MultiPolygon<f64>,
    phase_id: Option<&str>,
) -> Result<(CellLayer, u64), String> {
    enforce_max_cells(pool, project_id, area, spec).await?;
    let generated = generate_grid(area, spec).map_err(|e| e.to_string())?;
    let cells = cells_from_grid(&generated);

    let layer = CellLayer::new(project_id, aoi_layer_id, name, spec).with_phase(phase_id);
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("iniciando transação: {e}"))?;
    let saved = PgCellLayerRepo::create_on(&mut *tx, &layer)
        .await
        .map_err(|e| cell_layer_create_error(&e, name))?;
    let inserted = PgCellRepo::bulk_insert_on(&mut tx, &saved.id, &cells, spec.srid)
        .await
        .map_err(|e| format!("gravando células: {e}"))?;
    tx.commit()
        .await
        .map_err(|e| format!("commit das células: {e}"))?;
    Ok((saved, inserted))
}

/// Aplica a política de escala do projeto: `projects.max_cells` é o teto de
/// células **candidatas** da grade (o teto de memória do motor continua
/// valendo por cima). `max_cells < 0` desliga o teto de negócio.
///
/// Função única usada tanto pelo `grid create` quanto pelo caminho do seed
/// (`persist_grid`), para que a política do projeto não divirja.
pub async fn enforce_max_cells(
    pool: &PgPool,
    project_id: &str,
    area: &MultiPolygon<f64>,
    spec: &GridSpec,
) -> Result<(), String> {
    let max_cells: i32 = sqlx::query_scalar("SELECT max_cells FROM projects WHERE id = $1::uuid")
        .bind(project_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("lendo max_cells do projeto: {e}"))?;
    let needed = candidate_count(area, spec).map_err(|e| e.to_string())?;
    if max_cells >= 0 && needed > max_cells as u64 {
        return Err(format!(
            "grade grande demais para o projeto: {needed} células candidatas \
             excedem max_cells={max_cells} (aumente max_cells ou a resolução)"
        ));
    }
    Ok(())
}

/// Resolve e valida que uma fase pertence ao projeto; devolve o id canônico.
/// Erro acionável quando a fase não existe ou é de outro projeto.
async fn ensure_phase_in_project(
    pool: &PgPool,
    project_id: &str,
    phase_id: &str,
) -> Result<String, String> {
    let phase = PgPhaseRepo::new(pool.clone())
        .get(phase_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("fase `{phase_id}` não existe"))?;
    if phase.project_id != project_id {
        return Err(format!(
            "fase `{phase_id}` pertence a outro projeto ({}); use uma fase deste projeto",
            phase.project_id
        ));
    }
    Ok(phase.id)
}

/// Erro de criação de fase: nome duplicado no projeto vira mensagem acionável
/// (a única colisão esperada é o `UNIQUE(project_id, name)`).
fn phase_create_error(err: &ta_db::Error, name: &str) -> String {
    if let ta_db::Error::Sql(sqlx::Error::Database(db)) = err {
        if db.is_unique_violation() {
            return format!(
                "já existe uma fase com o nome `{name}` neste projeto; escolha outro nome em --name"
            );
        }
    }
    format!("criar fase `{name}`: {err}")
}

/// Erro de criação da camada de células: nome duplicado no projeto vira
/// mensagem acionável (a única colisão esperada é o `UNIQUE(project_id, name)`).
fn cell_layer_create_error(err: &ta_db::Error, name: &str) -> String {
    if let ta_db::Error::Sql(sqlx::Error::Database(db)) = err {
        if db.is_unique_violation() {
            return format!(
                "já existe uma camada de células com o nome `{name}` neste projeto; \
                 escolha outro nome em --name"
            );
        }
    }
    format!("criando camada de células: {err}")
}

/// Gera as células (motor puro) já no formato persistível (`ta_core::cell::Cell`).
pub fn generate_cells(area: &MultiPolygon<f64>, spec: &GridSpec) -> Result<Vec<Cell>, String> {
    let generated = generate_grid(area, spec).map_err(|e| e.to_string())?;
    Ok(cells_from_grid(&generated))
}

fn cells_from_grid(generated: &[ta_engine::GridCell]) -> Vec<Cell> {
    generated
        .iter()
        .map(|c| Cell {
            id: 0,
            cell_layer_id: String::new(),
            cell_id: c.cell_id.clone(),
            geom: c.geom.clone(),
            row_index: c.row_index,
            col_index: c.col_index,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Fase 2 — classes, grupos, rascunhos, rulesets e regras.
// ---------------------------------------------------------------------------

/// `class group create` — cria um grupo de classes no projeto.
pub async fn class_group_create(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    sort_order: i32,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let mut group = ClassGroup::new(&project.id, name);
    group.sort_order = sort_order;
    let saved = PgClassGroupRepo::new(pool.clone())
        .create(&group)
        .await
        .map_err(|e| duplicate_error(&e, "grupo de classes", name))?;
    println!(
        "grupo criado: {}\tprojeto={}\t{}",
        saved.id, project.name, saved.name
    );
    pool.close().await;
    Ok(())
}

/// `class group list` — lista os grupos do projeto.
pub async fn class_group_list(dsn: Option<&str>, project: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let groups = PgClassGroupRepo::new(pool.clone())
        .list_for_project(&project.id)
        .await
        .map_err(|e| e.to_string())?;
    if groups.is_empty() {
        println!("(nenhum grupo de classes no projeto `{}`)", project.name);
    }
    for group in &groups {
        println!("{}\tsort={}\t{}", group.id, group.sort_order, group.name);
    }
    pool.close().await;
    Ok(())
}

/// `class group rename` — renomeia um grupo (id ou nome).
pub async fn class_group_rename(
    dsn: Option<&str>,
    project: &str,
    group: &str,
    name: &str,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let mut existing = resolve_group(&pool, &project.id, group).await?;
    existing.name = name.to_owned();
    let saved = PgClassGroupRepo::new(pool.clone())
        .update(&existing)
        .await
        .map_err(|e| duplicate_error(&e, "grupo de classes", name))?
        .ok_or_else(|| format!("grupo `{group}` não existe mais"))?;
    println!("grupo renomeado: {}\t{}", saved.id, saved.name);
    pool.close().await;
    Ok(())
}

/// `class group delete` — remove um grupo; as classes ficam sem grupo.
pub async fn class_group_delete(
    dsn: Option<&str>,
    project: &str,
    group: &str,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let existing = resolve_group(&pool, &project.id, group).await?;
    let removed = PgClassGroupRepo::new(pool.clone())
        .delete(&existing.id)
        .await
        .map_err(|e| format!("remover grupo `{}`: {e}", existing.name))?;
    if !removed {
        return Err(format!("grupo `{group}` não existe mais"));
    }
    println!(
        "grupo removido: {}\t{}\t(as classes ficaram sem grupo)",
        existing.id, existing.name
    );
    pool.close().await;
    Ok(())
}

/// `class create` — cria uma classe no projeto.
// All flags of `class create` map 1:1 to parameters; a struct here would only
// hide the CLI shape.
#[allow(clippy::too_many_arguments)]
pub async fn class_create(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    group: Option<&str>,
    color: Option<&str>,
    parent: Option<&str>,
    sort_order: i32,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    validate_color(color)?;

    let group_id = match group {
        Some(group) => Some(resolve_group(&pool, &project.id, group).await?.id),
        None => None,
    };
    let parent_id = match parent {
        Some(parent) => Some(resolve_class(&pool, &project.id, parent).await?.id),
        None => None,
    };

    let mut class = Class::new(&project.id, name)
        .with_group(group_id.as_deref())
        .with_color(color)
        .with_parent(parent_id.as_deref());
    class.sort_order = sort_order;
    let saved = PgClassRepo::new(pool.clone())
        .create(&class)
        .await
        .map_err(|e| duplicate_error(&e, "classe", name))?;
    println!(
        "classe criada: {}\tprojeto={}\t{}\tcor={}",
        saved.id,
        project.name,
        saved.name,
        saved.color.as_deref().unwrap_or("(sem cor)")
    );
    pool.close().await;
    Ok(())
}

/// `class list` — lista as classes do projeto (com o nome do grupo).
pub async fn class_list(dsn: Option<&str>, project: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let groups = PgClassGroupRepo::new(pool.clone())
        .list_for_project(&project.id)
        .await
        .map_err(|e| e.to_string())?;
    let group_names: std::collections::BTreeMap<String, String> = groups
        .into_iter()
        .map(|group| (group.id, group.name))
        .collect();
    let classes = PgClassRepo::new(pool.clone())
        .list_for_project(&project.id)
        .await
        .map_err(|e| e.to_string())?;
    if classes.is_empty() {
        println!("(nenhuma classe no projeto `{}`)", project.name);
    }
    for class in &classes {
        let group = class
            .class_group_id
            .as_ref()
            .and_then(|id| group_names.get(id))
            .map(String::as_str)
            .unwrap_or("(sem grupo)");
        println!(
            "{}\tgrupo={group}\tat={}\tsort={}\tcor={}\t{}",
            class.id,
            if class.is_active { "sim" } else { "nao" },
            class.sort_order,
            class.color.as_deref().unwrap_or("(sem cor)"),
            class.name
        );
    }
    pool.close().await;
    Ok(())
}

/// `class update` — altera nome/grupo/cor/classe-mãe/atividade de uma classe.
#[allow(clippy::too_many_arguments)]
pub async fn class_update(
    dsn: Option<&str>,
    project: &str,
    class: &str,
    name: Option<String>,
    group: Option<String>,
    color: Option<String>,
    parent: Option<String>,
    active: Option<bool>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let mut existing = resolve_class(&pool, &project.id, class).await?;

    if let Some(name) = name {
        existing.name = name;
    }
    if let Some(group) = group {
        existing.class_group_id = Some(resolve_group(&pool, &project.id, &group).await?.id);
    }
    if let Some(color) = color {
        validate_color(Some(&color))?;
        existing.color = Some(color);
    }
    if let Some(parent) = parent {
        let parent = resolve_class(&pool, &project.id, &parent).await?;
        if parent.id == existing.id {
            return Err("uma classe não pode ser mãe de si mesma".to_owned());
        }
        existing.parent_id = Some(parent.id);
    }
    if let Some(active) = active {
        // Nota (F8): `classes.is_active` hoje é atributo de EXIBIÇÃO — o
        // motor de regras não consulta classes; desativar não altera
        // decisões de classificação.
        existing.is_active = active;
    }

    let saved = PgClassRepo::new(pool.clone())
        .update(&existing)
        .await
        .map_err(|e| duplicate_error(&e, "classe", &existing.name))?
        .ok_or_else(|| format!("classe `{class}` não existe mais"))?;
    println!("classe atualizada: {}\t{}", saved.id, saved.name);
    pool.close().await;
    Ok(())
}

/// `class delete` — remove uma classe.
///
/// Pré-checagem mostra a contagem de regras que decidem a classe E das
/// células que ela decide em rascunho(s) (`draft_cells` é
/// `ON DELETE RESTRICT` desde a migração `0013`): sem `--force`, o delete é
/// **bloqueado** com a mensagem acionável; com `--force`, as decisões de
/// rascunho, as regras e a classe caem numa **única transação**, com as
/// três contagens reportadas (intenção explícita do operador).
pub async fn class_delete(
    dsn: Option<&str>,
    project: &str,
    class: &str,
    force: bool,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let existing = resolve_class(&pool, &project.id, class).await?;

    let repo = PgClassRepo::new(pool.clone());
    let rules_using = PgRuleRepo::new(pool.clone())
        .count_for_class(&existing.id)
        .await
        .map_err(|e| e.to_string())?;
    let drafts_using = repo
        .count_drafts_for_class(&existing.id)
        .await
        .map_err(|e| e.to_string())?;
    let cells_in_drafts: i64 = drafts_using.iter().map(|(_, cells)| cells).sum();

    if !force && (rules_using > 0 || cells_in_drafts > 0) {
        pool.close().await;
        if cells_in_drafts > 0 {
            let names: Vec<&str> = drafts_using.iter().map(|(name, _)| name.as_str()).collect();
            return Err(format!(
                "classe `{}` decide {} célula(s) em {} rascunho(s): {}; apague \
                 o(s) rascunho(s) com `draft delete` ou repita com --force",
                existing.name,
                cells_in_drafts,
                drafts_using.len(),
                names.join(", ")
            ));
        }
        return Err(format!(
            "classe `{}` é usada por {} regra(s); remova/reaponte as regras \
             (`rule remove`) ou desative a classe, ou repita com --force para \
             apagar as regras junto",
            existing.name, rules_using
        ));
    }
    if force {
        // `--force`: decisões de rascunho + regras + classe numa transação
        // única, com as três contagens reportadas.
        let report = repo
            .delete_force(&existing.id)
            .await
            .map_err(|e| class_delete_error(&e, &existing.name))?;
        if !report.class {
            pool.close().await;
            return Err(format!("classe `{class}` não existe mais"));
        }
        println!(
            "classe removida: {}\t{}\t(decisões em rascunho apagadas: {}, \
             regras apagadas: {} — transação única)",
            existing.id, existing.name, report.draft_cells, report.rules
        );
        pool.close().await;
        return Ok(());
    }
    let removed = repo
        .delete(&existing.id)
        .await
        .map_err(|e| class_delete_error(&e, &existing.name))?;
    if !removed {
        pool.close().await;
        return Err(format!("classe `{class}` não existe mais"));
    }
    println!("classe removida: {}\t{}", existing.id, existing.name);
    pool.close().await;
    Ok(())
}

/// Erro de remoção de classe: a corrida restante (regra criada entre a
/// pré-checagem e o delete) vira a mesma mensagem acionável do repo.
fn class_delete_error(err: &ta_db::Error, name: &str) -> String {
    if let ta_db::Error::ClassInUseByRules { count, .. } = err {
        return format!(
            "classe `{name}` é usada por {count} regra(s); remova/reaponte as \
             regras (`rule remove`) ou desative a classe, ou repita com \
             --force para apagar as regras junto"
        );
    }
    format!("remover classe `{name}`: {err}")
}

/// `draft create` — cria uma camada de rascunho de classificação.
pub async fn draft_create(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    phase: Option<&str>,
    cell_layer: Option<&str>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let phase_id = match phase {
        Some(phase) => Some(ensure_phase_in_project(&pool, &project.id, phase).await?),
        None => None,
    };
    let cell_layer_id = match cell_layer {
        Some(cell_layer) => {
            Some(ensure_cell_layer_in_project(&pool, &project.id, cell_layer).await?)
        }
        None => None,
    };
    let draft = DraftLayer::new(&project.id, name)
        .with_phase(phase_id.as_deref())
        .with_cell_layer(cell_layer_id.as_deref());
    let saved = PgDraftLayerRepo::new(pool.clone())
        .create(&draft)
        .await
        .map_err(|e| duplicate_error(&e, "rascunho", name))?;
    println!(
        "rascunho criado: {}\tprojeto={}\testado={}",
        saved.id, project.name, saved.state
    );
    pool.close().await;
    Ok(())
}

/// `draft list` — lista os rascunhos do projeto.
pub async fn draft_list(dsn: Option<&str>, project: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let drafts = PgDraftLayerRepo::new(pool.clone())
        .list_for_project(&project.id)
        .await
        .map_err(|e| e.to_string())?;
    if drafts.is_empty() {
        println!("(nenhum rascunho no projeto `{}`)", project.name);
    }
    for draft in &drafts {
        println!(
            "{}\testado={}\tfase={}\tgrade={}\t{}",
            draft.id,
            draft.state,
            draft.phase_id.as_deref().unwrap_or("(sem fase)"),
            draft.cell_layer_id.as_deref().unwrap_or("(sem grade)"),
            draft.name
        );
    }
    pool.close().await;
    Ok(())
}

/// `draft delete` — apaga o rascunho e as células classificadas nele
/// (`draft_cells` cai por CASCADE, migração 0013).
///
/// O rascunho é derivado (reclassificar recria), mas a remoção é
/// **destrutiva**: a contagem de células classificadas é mostrada ANTES e,
/// sem `--yes`, o delete é **bloqueado** — intenção explícita do operador.
pub async fn draft_delete(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    yes: bool,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let drafts = PgDraftLayerRepo::new(pool.clone());
    let draft = drafts
        .list_for_project(&project.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|d| d.name == name)
        .ok_or_else(|| {
            format!(
                "rascunho `{name}` não existe no projeto `{}`; veja `draft list`",
                project.name
            )
        })?;
    let cells = drafts
        .count_cells(&draft.id)
        .await
        .map_err(|e| e.to_string())?;
    if !yes {
        pool.close().await;
        return Err(format!(
            "rascunho `{}` guarda {} célula(s) classificada(s); apagar o \
             rascunho apaga também essas decisões — repita com --yes para \
             confirmar",
            draft.name, cells
        ));
    }
    let removed = drafts
        .delete(&draft.id)
        .await
        .map_err(|e| format!("remover rascunho `{}`: {e}", draft.name))?;
    if !removed {
        pool.close().await;
        return Err(format!("rascunho `{name}` não existe mais"));
    }
    println!(
        "rascunho apagado: {}\t{}\t(células classificadas removidas: {cells})",
        draft.id, draft.name
    );
    pool.close().await;
    Ok(())
}

/// `ruleset create` — grava a próxima versão da linhagem `(projeto, nome)`.
pub async fn ruleset_create(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    description: Option<String>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let ruleset = RuleSet::new(&project.id, name).with_description(description.as_deref());
    let saved = PgRuleSetRepo::new(pool.clone())
        .create(&ruleset)
        .await
        .map_err(|e| duplicate_error(&e, "ruleset", name))?;
    println!(
        "ruleset criado: {}\tprojeto={}\t{}\tversao={}",
        saved.id, project.name, saved.name, saved.version
    );
    pool.close().await;
    Ok(())
}

/// `ruleset list` — lista as versões dos rulesets do projeto.
pub async fn ruleset_list(dsn: Option<&str>, project: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let rulesets = PgRuleSetRepo::new(pool.clone())
        .list_for_project(&project.id)
        .await
        .map_err(|e| e.to_string())?;
    if rulesets.is_empty() {
        println!("(nenhum ruleset no projeto `{}`)", project.name);
    }
    for ruleset in &rulesets {
        println!(
            "{}\t{}\tversao={}\tdescricao={}",
            ruleset.id,
            ruleset.name,
            ruleset.version,
            ruleset.description.as_deref().unwrap_or("(sem descricao)")
        );
    }
    pool.close().await;
    Ok(())
}

/// `ruleset show` — mostra a versão (padrão: mais recente) e suas regras.
pub async fn ruleset_show(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    version: Option<i32>,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .find(&project.id, name, version)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "ruleset `{name}`{} não existe no projeto `{}`; crie com `ruleset create`",
                version
                    .map(|v| format!(" (versão {v})"))
                    .unwrap_or_default(),
                project.name
            )
        })?;
    println!(
        "ruleset: {}\t{}\tversao={}\tdescricao={}",
        ruleset.id,
        ruleset.name,
        ruleset.version,
        ruleset.description.as_deref().unwrap_or("(sem descricao)")
    );
    let classes = class_names(&pool, &project.id).await?;
    let rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset.id)
        .await
        .map_err(|e| e.to_string())?;
    if rules.is_empty() {
        println!("(nenhuma regra neste ruleset)");
    }
    for rule in &rules {
        println!(
            "  {}\tprioridade={}\tat={}\tclasse={}\tquando={}",
            rule.name,
            rule.priority,
            if rule.is_active { "sim" } else { "nao" },
            classes
                .get(&rule.class_id)
                .map(String::as_str)
                .unwrap_or(&rule.class_id),
            rule.predicate
        );
    }
    pool.close().await;
    Ok(())
}

/// `ruleset delete` — remove um ruleset (as regras caem em cascata).
///
/// Pré-checagem de `draft_cells` (FK `ON DELETE RESTRICT` da 0013): ruleset
/// que decide células classificadas é RECUSADO sem `--force`, com contagem e
/// nomes dos rascunhos; com `--force`, decisões de rascunho + regras +
/// ruleset caem numa **única transação**, com as três contagens reportadas —
/// nada silencioso.
pub async fn ruleset_delete(
    dsn: Option<&str>,
    project: &str,
    name: &str,
    version: Option<i32>,
    force: bool,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let repo = PgRuleSetRepo::new(pool.clone());
    let ruleset = repo
        .find(&project.id, name, version)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "ruleset `{name}`{} não existe no projeto `{}`; veja `ruleset list`",
                version
                    .map(|v| format!(" (versão {v})"))
                    .unwrap_or_default(),
                project.name
            )
        })?;
    let drafts_using = repo
        .count_drafts_for_ruleset(&ruleset.id)
        .await
        .map_err(|e| e.to_string())?;
    let cells_in_drafts: i64 = drafts_using.iter().map(|(_, cells)| cells).sum();
    if cells_in_drafts > 0 && !force {
        pool.close().await;
        let names: Vec<&str> = drafts_using.iter().map(|(name, _)| name.as_str()).collect();
        return Err(format!(
            "ruleset `{}` (v{}) decide {} célula(s) em {} rascunho(s): {}; \
             apague o(s) rascunho(s) com `draft delete` ou repita com --force",
            ruleset.name,
            ruleset.version,
            cells_in_drafts,
            drafts_using.len(),
            names.join(", ")
        ));
    }
    if force {
        let report = repo
            .delete_force(&ruleset.id)
            .await
            .map_err(|e| format!("remover ruleset `{}`: {e}", ruleset.name))?;
        if !report.ruleset {
            pool.close().await;
            return Err(format!("ruleset `{name}` não existe mais"));
        }
        println!(
            "ruleset removido: {}\t{}\tv{}\t(decisões em rascunho apagadas: {}, \
             regras apagadas: {} — transação única)",
            ruleset.id, ruleset.name, ruleset.version, report.draft_cells, report.rules
        );
        pool.close().await;
        return Ok(());
    }
    let rules_before = PgRuleRepo::new(pool.clone())
        .count_for_ruleset(&ruleset.id)
        .await
        .map_err(|e| e.to_string())?;
    let removed = repo
        .delete(&ruleset.id)
        .await
        .map_err(|e| format!("remover ruleset `{}`: {e}", ruleset.name))?;
    if !removed {
        pool.close().await;
        return Err(format!("ruleset `{name}` não existe mais"));
    }
    println!(
        "ruleset removido: {}\t{}\tv{}\t(regras removidas em cascata: {rules_before})",
        ruleset.id, ruleset.name, ruleset.version
    );
    pool.close().await;
    Ok(())
}

/// `rule add` — valida o predicado e grava a regra no ruleset.
pub async fn rule_add(
    dsn: Option<&str>,
    ruleset_id: &str,
    class: &str,
    name: &str,
    priority: i32,
    predicate: &str,
) -> Result<(), String> {
    // Validação do predicado ANTES de tocar no banco: não gravar lixo.
    parse_predicate(predicate).map_err(|e| format!("predicado inválido: {e}"))?;

    let pool = crate::open_pool(dsn).await?;
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .get(ruleset_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("ruleset `{ruleset_id}` não existe"))?;
    let class = resolve_class(&pool, &ruleset.project_id, class).await?;
    let rule = Rule::new(&ruleset.id, &class.id, name, predicate).with_priority(priority);
    let saved = PgRuleRepo::new(pool.clone())
        .create(&rule)
        .await
        .map_err(|e| rule_create_error(&e, name, class.name.as_str()))?;
    println!(
        "regra adicionada: {}\truleset={} (v{})\tclasse={}\tprioridade={}",
        saved.id, ruleset.name, ruleset.version, class.name, saved.priority
    );
    pool.close().await;
    Ok(())
}

/// `rule list` — lista as regras do ruleset (maior prioridade primeiro).
pub async fn rule_list(dsn: Option<&str>, ruleset_id: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .get(ruleset_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("ruleset `{ruleset_id}` não existe"))?;
    let classes = class_names(&pool, &ruleset.project_id).await?;
    let rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset.id)
        .await
        .map_err(|e| e.to_string())?;
    if rules.is_empty() {
        println!("(nenhuma regra no ruleset `{}`)", ruleset.name);
    }
    for rule in &rules {
        println!(
            "{}\tprioridade={}\tat={}\tclasse={}\tquando={}",
            rule.name,
            rule.priority,
            if rule.is_active { "sim" } else { "nao" },
            classes
                .get(&rule.class_id)
                .map(String::as_str)
                .unwrap_or(&rule.class_id),
            rule.predicate
        );
    }
    pool.close().await;
    Ok(())
}

/// `rule remove` — remove uma regra pelo nome dentro do ruleset.
pub async fn rule_remove(dsn: Option<&str>, ruleset_id: &str, name: &str) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(ruleset_id)
        .await
        .map_err(|e| e.to_string())?;
    let rule = rules
        .into_iter()
        .find(|rule| rule.name == name)
        .ok_or_else(|| {
            format!("regra `{name}` não existe no ruleset `{ruleset_id}`; veja `rule list`")
        })?;
    if !PgRuleRepo::new(pool.clone())
        .delete(&rule.id)
        .await
        .map_err(|e| format!("remover regra `{name}`: {e}"))?
    {
        return Err(format!("regra `{name}` não existe mais"));
    }
    println!("regra removida: {}\t{}", rule.id, rule.name);
    pool.close().await;
    Ok(())
}

/// `rule update` — atualização **parcial** de uma regra identificada por
/// `--ruleset` + `--name`: apenas as flags presentes alteram o campo (o
/// update no repositório preserva os ausentes, nunca zera).
pub async fn rule_update(
    dsn: Option<&str>,
    ruleset_id: &str,
    name: &str,
    class: Option<String>,
    priority: Option<i32>,
    when: Option<String>,
    active: Option<bool>,
) -> Result<(), String> {
    if class.is_none() && priority.is_none() && when.is_none() && active.is_none() {
        return Err(
            "nada para atualizar em `rule update`: informe --class, --priority, \
             --when ou --active"
                .to_owned(),
        );
    }
    // Validação do predicado ANTES de tocar no banco: não gravar lixo.
    if let Some(predicate) = &when {
        parse_predicate(predicate).map_err(|e| format!("predicado inválido: {e}"))?;
    }
    let pool = crate::open_pool(dsn).await?;
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .get(ruleset_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("ruleset `{ruleset_id}` não existe"))?;
    let class_id = match &class {
        Some(class) => Some(resolve_class(&pool, &ruleset.project_id, class).await?.id),
        None => None,
    };
    let patch = RulePatch {
        class_id,
        priority,
        predicate: when,
        is_active: active,
    };
    let updated = PgRuleRepo::new(pool.clone())
        .update_partial(&ruleset.id, name, &patch)
        .await
        .map_err(|e| format!("atualizar regra `{name}`: {e}"))?
        .ok_or_else(|| {
            format!(
                "regra `{name}` não existe no ruleset `{}`; veja `rule list`",
                ruleset.name
            )
        })?;
    let classes = class_names(&pool, &ruleset.project_id).await?;
    println!(
        "regra atualizada: {}\truleset={} (v{})\tclasse={}\tprioridade={}\tat={}\tquando={}",
        updated.id,
        ruleset.name,
        ruleset.version,
        classes
            .get(&updated.class_id)
            .map(String::as_str)
            .unwrap_or(&updated.class_id),
        updated.priority,
        if updated.is_active { "sim" } else { "nao" },
        updated.predicate
    );
    pool.close().await;
    Ok(())
}

/// `rule eval` — avalia o ruleset (motor puro) contra medidas observadas.
pub async fn rule_eval(dsn: Option<&str>, ruleset_id: &str, obs: &str) -> Result<(), String> {
    let measures = parse_observations(obs)?;
    let pool = crate::open_pool(dsn).await?;
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .get(ruleset_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("ruleset `{ruleset_id}` não existe"))?;
    let rules = PgRuleRepo::new(pool.clone())
        .list_for_ruleset(&ruleset.id)
        .await
        .map_err(|e| e.to_string())?;
    let decision = evaluate(&rules, &measures);

    match decision.class_id.as_deref() {
        Some(class_id) => {
            let class = PgClassRepo::new(pool.clone())
                .get(class_id)
                .await
                .map_err(|e| e.to_string())?;
            let class_name = class.as_ref().map_or(class_id, |c| c.name.as_str());
            let winner = rules
                .iter()
                .find(|rule| Some(rule.id.as_str()) == decision.rule_id.as_deref());
            println!(
                "decisão: classe={class_name} ({class_id})\tregra={}\tprioridade={}",
                winner.map_or("(desconhecida)", |rule| rule.name.as_str()),
                winner.map_or(0, |rule| rule.priority)
            );
        }
        None => println!("decisão: nenhuma classe"),
    }
    println!("  regras avaliadas: {}", decision.evaluated);
    if decision.skipped_invalid > 0 {
        println!(
            "  regras ignoradas (predicado inválido): {} [{}]",
            decision.skipped_invalid,
            decision.skipped_invalid_names.join(", ")
        );
    }
    if decision.missing_measures.is_empty() {
        println!("  medidas ausentes: (nenhuma)");
    } else {
        println!(
            "  medidas ausentes: {}",
            decision.missing_measures.join(", ")
        );
    }
    pool.close().await;
    Ok(())
}

/// Resolve um grupo por UUID ou nome dentro do projeto.
async fn resolve_group(
    pool: &PgPool,
    project_id: &str,
    id_or_name: &str,
) -> Result<ClassGroup, String> {
    let repo = PgClassGroupRepo::new(pool.clone());
    if looks_uuid(id_or_name) {
        if let Some(group) = repo.get(id_or_name).await.map_err(|e| e.to_string())? {
            if group.project_id != project_id {
                return Err(format!(
                    "grupo `{id_or_name}` pertence a outro projeto ({}); use um grupo deste projeto",
                    group.project_id
                ));
            }
            return Ok(group);
        }
    }
    repo.list_for_project(project_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|group| group.name == id_or_name)
        .ok_or_else(|| {
            format!("grupo `{id_or_name}` não existe neste projeto; crie com `class group create`")
        })
}

/// Resolve uma classe por UUID ou nome dentro do projeto.
async fn resolve_class(pool: &PgPool, project_id: &str, id_or_name: &str) -> Result<Class, String> {
    let repo = PgClassRepo::new(pool.clone());
    if looks_uuid(id_or_name) {
        if let Some(class) = repo.get(id_or_name).await.map_err(|e| e.to_string())? {
            if class.project_id != project_id {
                return Err(format!(
                    "classe `{id_or_name}` pertence a outro projeto ({}); use uma classe deste projeto",
                    class.project_id
                ));
            }
            return Ok(class);
        }
    }
    repo.list_for_project(project_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|class| class.name == id_or_name)
        .ok_or_else(|| {
            format!("classe `{id_or_name}` não existe neste projeto; crie com `class create`")
        })
}

/// Valida que a camada de células é do projeto e devolve o id canônico.
pub(crate) async fn ensure_cell_layer_in_project(
    pool: &PgPool,
    project_id: &str,
    cell_layer_id: &str,
) -> Result<String, String> {
    let layer = PgCellLayerRepo::new(pool.clone())
        .get(cell_layer_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("camada de células `{cell_layer_id}` não existe"))?;
    if layer.project_id != project_id {
        return Err(format!(
            "camada de células `{cell_layer_id}` pertence a outro projeto ({}); \
             use uma grade deste projeto",
            layer.project_id
        ));
    }
    Ok(layer.id)
}

/// Mapa `classes.id → nome` para exibição.
async fn class_names(
    pool: &PgPool,
    project_id: &str,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    Ok(PgClassRepo::new(pool.clone())
        .list_for_project(project_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|class| (class.id, class.name))
        .collect())
}

/// `#RRGGBB` (7 bytes, `#` + 6 hex). A mesma regra do CHECK da migração 0009.
fn validate_color(color: Option<&str>) -> Result<(), String> {
    let Some(color) = color else {
        return Ok(());
    };
    let bytes = color.as_bytes();
    let ok = bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "cor inválida `{color}`: use o formato hexadecimal #RRGGBB (ex.: #1A7F3C)"
        ))
    }
}

fn looks_uuid(value: &str) -> bool {
    value.len() == 36 && value.as_bytes()[8] == b'-'
}

/// Interpreta `--obs nome=valor,nome=valor` (nomes em minúsculas).
pub fn parse_observations(spec: &str) -> Result<std::collections::BTreeMap<String, f64>, String> {
    if spec.trim().is_empty() {
        return Err("--obs vazio: informe medidas como `ndvi=0.7,nbr=0.05`".to_owned());
    }
    let mut measures = std::collections::BTreeMap::new();
    for part in spec.split(',') {
        let (name, value) = part.split_once('=').ok_or_else(|| {
            format!(
                "medida inválida `{}`: use `nome=valor` (ex.: ndvi=0.7)",
                part.trim()
            )
        })?;
        let name = name.trim().to_ascii_lowercase();
        if !is_measure_name(&name) {
            return Err(format!(
                "nome de medida inválido `{name}`: use letras minúsculas, dígitos e `_` \
                 (começando por letra ou `_`)"
            ));
        }
        let value: f64 = value.trim().parse().map_err(|_| {
            format!(
                "valor inválido `{}` para a medida `{name}`: use um número (ex.: 0.7)",
                value.trim()
            )
        })?;
        if !value.is_finite() {
            return Err(format!("valor da medida `{name}` não é finito: {value}"));
        }
        if measures.insert(name.clone(), value).is_some() {
            return Err(format!("medida `{name}` repetida em --obs"));
        }
    }
    Ok(measures)
}

fn is_measure_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

/// Erro de escrita com nome duplicado vira mensagem acionável (a única
/// colisão esperada é o `UNIQUE(project_id, name)` / `UNIQUE(ruleset_id, name)`).
///
/// `VersionRace` NÃO é duplicidade — é colisão de versão restante sob o lock
/// de linhagem (F2) — e passa com a própria mensagem ("tente novamente"),
/// nunca com "escolha outro nome".
fn duplicate_error(err: &ta_db::Error, kind: &str, name: &str) -> String {
    if let ta_db::Error::VersionRace { .. } = err {
        return err.to_string();
    }
    if let ta_db::Error::Sql(sqlx::Error::Database(db)) = err {
        if db.is_unique_violation() {
            return format!(
                "já existe um(a) {kind} com o nome `{name}`; escolha outro nome em --name"
            );
        }
    }
    // Sem verbo ("criar"): o mesmo mapa serve a `update` (erro tipado de
    // ciclo/clone sai com a própria mensagem, não com "criar ...").
    format!("{kind} `{name}`: {err}")
}

fn rule_create_error(err: &ta_db::Error, name: &str, class: &str) -> String {
    if let ta_db::Error::Sql(sqlx::Error::Database(db)) = err {
        if db.is_unique_violation() {
            return format!(
                "já existe uma regra com o nome `{name}` neste ruleset; \
                 escolha outro nome em --name"
            );
        }
    }
    format!("criar regra `{name}` para a classe `{class}`: {err}")
}

/// `project delete` — apaga o projeto INTEIRO: deleção ORDENADA em UMA
/// transação (decisões → regras → rulesets → fila → medidas → células →
/// grades → rascunhos → camadas → AOI → vínculos de cena → classes → grupos
/// → fases → projeto), respeitando as FK `ON DELETE RESTRICT` SEM desativar
/// triggers, com RELATÓRIO de contagens por tabela. Cenas (`scenes`) são
/// globais: sai só o vínculo do projeto, a cena permanece.
///
/// Sem `--yes`: mostra o resumo do que seria apagado (contagens) e RECUSA.
pub async fn project_delete(dsn: Option<&str>, project: &str, yes: bool) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let repo = PgProjectRepo::new(pool.clone());
    // Deleção NÃO auto-cria projeto (diferente do `resolve_project` dos
    // comandos de escrita): resolve só EXISTENTE, por id (UUID) ou nome.
    let proj = {
        let mut found: Option<ta_core::project::Project> = None;
        if project.len() == 36 && project.as_bytes()[8] == b'-' {
            found = repo.get(project).await.map_err(|e| e.to_string())?;
        }
        if found.is_none() {
            found = repo
                .list()
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|p| p.name == project);
        }
        found.ok_or_else(|| {
            format!("projeto `{project}` não existe; liste com `ta-cli list-layers`")
        })?
    };
    if !yes {
        let report = repo
            .delete_preview(&proj.id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("projeto `{}` sumiu durante o preview", proj.name))?;
        let resumo = report
            .entries()
            .iter()
            .map(|(tabela, n)| format!("{tabela}={n}"))
            .collect::<Vec<_>>()
            .join(" ");
        pool.close().await;
        return Err(format!(
            "recusado sem --yes: apagar o projeto `{}` remove {} linha(s): \
             {resumo}; cenas globais PERMANECEM (sai só o vínculo do projeto). \
             Repita com --yes para confirmar",
            proj.name,
            report.total()
        ));
    }
    let report = repo
        .delete_ordered(&proj.id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("projeto `{}` não existe mais", proj.name))?;
    println!(
        "projeto apagado: {} ({})\ntotal={}\n{}",
        proj.name,
        proj.id,
        report.total(),
        report
            .entries()
            .iter()
            .map(|(tabela, n)| format!("{tabela}={n}"))
            .collect::<Vec<_>>()
            .join("\t")
    );
    pool.close().await;
    Ok(())
}
