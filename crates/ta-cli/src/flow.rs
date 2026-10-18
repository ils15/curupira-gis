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
use ta_core::feature::Feature;
use ta_core::phase::Phase;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{
    BlockedCellRepo, CellLayerRepo, CellRepo, PgBlockedCellRepo, PgCellLayerRepo, PgCellRepo,
};
use ta_db::repo_phase::{PgPhaseRepo, PhaseRepo};
use ta_db::spatial::{TARGET_SRID, srid_units, transform_geometry};
use ta_engine::{aoi_bbox, candidate_count, generate_grid};

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
