//! **Preparo do fluxo — grade de células** (extraído do `ta-cli` na Fase 5b).
//!
//! A geração da grade é **do motor puro** `ta-engine` (sem IO/DB); aqui ficam
//! a leitura da AOI e a costura transacional (camada + células no MESMO
//! commit), a política de escala do projeto (`projects.max_cells`) e a
//! reprojeção da AOI para o SRS da grade (PostGIS `ST_Transform`).
//!
//! `resolution`/origem são expressas nas **unidades de `srid`** (graus se
//! geográfico, ex. 4674; metros se projetado, ex. 5880). As células são
//! **sempre persistidas em EPSG:4674** (armazenamento canônico). É o mesmo
//! caminho do `grid create` da CLI e do botão “Gerar grade” da GUI.

use geo_types::{Geometry, MultiPolygon, Polygon};
use ta_core::cell::{Cell, CellLayer, GridSpec};
use ta_core::feature::Feature;
use ta_db::repo::PgFeatureRepo;
use ta_db::repo::{FeatureRepo, PgProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{CellRepo, PgCellLayerRepo, PgCellRepo};
use ta_db::spatial::{TARGET_SRID, srid_units, transform_geometry};
use ta_db::sqlx::PgPool;
use ta_engine::{aoi_bbox, candidate_count, generate_grid};

use crate::error::FlowError;
use crate::phase;

/// Grade criada, com os números que a CLI/GUI exibem.
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedGrid {
    /// Camada de células persistida.
    pub layer: CellLayer,
    /// Células efetivamente inseridas nesta chamada (`ON CONFLICT DO NOTHING`).
    pub inserted: u64,
    /// Total de células da camada após a inserção.
    pub count: u64,
    /// Unidades de `resolution`/origem no SRS da grade (`metros`/`graus`/…).
    pub units: String,
}

/// `grid create` — gera a grade e persiste camada + células.
///
/// `origin_x`/`origin_y` ausentes usam o canto inferior esquerdo do envelope da
/// AOI **no SRS da grade**. `phase_id` opcional precisa ser do MESMO projeto da
/// AOI (erro acionável). `name` ausente vira `"grade"` (como na CLI).
#[allow(clippy::too_many_arguments)]
pub async fn create(
    pool: &PgPool,
    aoi_layer_id: &str,
    resolution: f64,
    origin_x: Option<f64>,
    origin_y: Option<f64>,
    srid: i32,
    name: Option<&str>,
    phase_id: Option<&str>,
) -> Result<CreatedGrid, FlowError> {
    // EPSG precisa existir em `spatial_ref_sys` (erro acionável antes de
    // qualquer reprojeção) e define as unidades da resolução/origem.
    let units = srid_units(pool, srid).await?;
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .get(aoi_layer_id)
        .await?
        .ok_or_else(|| FlowError::AoiNotFound(aoi_layer_id.to_owned()))?;

    // Geometria da AOI = todas as feições poligonais da camada vinculada,
    // lidas no SRS de armazenamento (4674).
    let features = PgFeatureRepo::new(pool.clone())
        .list(&aoi.layer_id)
        .await
        .map_err(FlowError::AoiGeometry)?;
    let area_storage = multipolygon_from_features(&features)?;
    // O recorte acontece no SRS da grade; a AOI é reprojetada 4674 → srid.
    let area = aoi_in_srid(pool, &area_storage, srid).await?;

    let origin_x = origin_x.unwrap_or_else(|| aoi_bbox(&area).map(|b| b[0]).unwrap_or(0.0));
    let origin_y = origin_y.unwrap_or_else(|| aoi_bbox(&area).map(|b| b[1]).unwrap_or(0.0));
    let spec = GridSpec::new(resolution, origin_x, origin_y, srid);

    let phase_id = match phase_id {
        Some(id) => Some(phase::ensure_in_project(pool, &aoi.project_id, id).await?),
        None => None,
    };
    let name = name.unwrap_or("grade");
    let (layer, inserted) = persist_grid(
        pool,
        &aoi.project_id,
        &aoi.id,
        name,
        &spec,
        &area,
        phase_id.as_deref(),
    )
    .await?;
    let count = PgCellRepo::new(pool.clone()).count(&layer.id).await?;
    Ok(CreatedGrid {
        layer,
        inserted,
        count,
        units,
    })
}

/// Converte as feições de uma camada em um [`MultiPolygon`] (AOI poligonal),
/// lidas no SRS de armazenamento (EPSG:4674). Feições não poligonais são
/// ignoradas; se não sobrar nenhuma, é erro.
pub fn multipolygon_from_features(features: &[Feature]) -> Result<MultiPolygon<f64>, FlowError> {
    let mut polygons: Vec<Polygon<f64>> = Vec::new();
    for feature in features {
        match &feature.geom {
            Geometry::Polygon(p) => polygons.push(p.clone()),
            Geometry::MultiPolygon(mp) => polygons.extend(mp.0.iter().cloned()),
            _ => {}
        }
    }
    if polygons.is_empty() {
        return Err(FlowError::AoiNoPolygon);
    }
    Ok(MultiPolygon::new(polygons))
}

/// AOI no SRS da grade: reprojeta 4674 → `srid` com PostGIS `ST_Transform`.
///
/// Quando `srid` já é o SRS de armazenamento (4674), devolve a geometria
/// **sem** round-trip (nenhuma transformação é aplicada desnecessariamente).
pub async fn aoi_in_srid(
    pool: &PgPool,
    area_4674: &MultiPolygon<f64>,
    srid: i32,
) -> Result<MultiPolygon<f64>, FlowError> {
    if srid == TARGET_SRID as i32 {
        return Ok(area_4674.clone());
    }
    let target = u32::try_from(srid).map_err(|_| FlowError::AoiReproject {
        srid,
        message: "EPSG inválido".to_owned(),
    })?;
    let geom = transform_geometry(
        pool,
        &Geometry::MultiPolygon(area_4674.clone()),
        TARGET_SRID,
        target,
    )
    .await
    .map_err(|e| FlowError::AoiReproject {
        srid,
        message: e.to_string(),
    })?;
    match geom {
        Geometry::MultiPolygon(mp) => Ok(mp),
        other => Err(FlowError::AoiReprojectUnexpected(format!("{other:?}"))),
    }
}

/// Gera a grade (motor puro) e persiste camada + células em UMA transação.
///
/// Devolve a camada criada e o número de células efetivamente inseridas. É
/// idempotente no nível das células (`ON CONFLICT DO NOTHING`). Aplica a mesma
/// política de `projects.max_cells` do caminho do CLI (`ensure_max_cells`),
/// para que a regra do projeto seja única.
///
/// **`ANALYZE` após o commit** (best-effort): as estatísticas do planner são
/// atualizadas quando a transação já está durável; uma falha aqui não pode
/// desfazer a grade (seria reportar erro para dado já gravado). Sem elas a
/// consulta de viewport varre a camada em vez de usar o GIST.
pub async fn persist_grid(
    pool: &PgPool,
    project_id: &str,
    aoi_layer_id: &str,
    name: &str,
    spec: &GridSpec,
    area: &MultiPolygon<f64>,
    phase_id: Option<&str>,
) -> Result<(CellLayer, u64), FlowError> {
    ensure_max_cells(pool, project_id, area, spec).await?;
    let cells = generate_cells(area, spec)?;

    let layer = CellLayer::new(project_id, aoi_layer_id, name, spec).with_phase(phase_id);
    let mut tx = pool.begin().await.map_err(|e| FlowError::GridTransaction {
        step: "iniciando transação",
        message: e.to_string(),
    })?;
    let saved = PgCellLayerRepo::create_on(&mut *tx, &layer)
        .await
        .map_err(|err| {
            if phase::is_unique_violation(&err) {
                FlowError::CellLayerNameTaken {
                    name: name.to_owned(),
                }
            } else {
                FlowError::CellLayerCreate(err)
            }
        })?;
    let inserted = PgCellRepo::bulk_insert_on(&mut tx, &saved.id, &cells, spec.srid)
        .await
        .map_err(FlowError::CellsInsert)?;
    tx.commit().await.map_err(|e| FlowError::GridTransaction {
        step: "commit das células",
        message: e.to_string(),
    })?;
    if inserted > 0 {
        // Best-effort pós-commit (ver doc da função): performance, não
        // integridade.
        let _ = PgCellRepo::analyze_cells_on(pool).await;
    }
    Ok((saved, inserted))
}

/// Aplica a política de escala do projeto: `projects.max_cells` é o teto de
/// células **candidatas** da grade (o teto de memória do motor continua
/// valendo por cima). `max_cells < 0` desliga o teto de negócio.
///
/// Função única usada pelo `grid create`, pelo seed e pela GUI, para que a
/// política do projeto não divirja.
pub async fn ensure_max_cells(
    pool: &PgPool,
    project_id: &str,
    area: &MultiPolygon<f64>,
    spec: &GridSpec,
) -> Result<(), FlowError> {
    let max_cells = PgProjectRepo::new(pool.clone())
        .flow_settings(project_id)
        .await?
        .ok_or_else(|| FlowError::ProjectNotFound(project_id.to_owned()))?
        .max_cells;
    let needed = candidate_count(area, spec).map_err(|e| FlowError::GridEngine(e.to_string()))?;
    if max_cells >= 0 && needed > max_cells as u64 {
        return Err(FlowError::MaxCellsExceeded {
            needed,
            max: max_cells,
        });
    }
    Ok(())
}

/// Gera as células (motor puro) já no formato persistível (`ta_core::cell::Cell`).
pub fn generate_cells(area: &MultiPolygon<f64>, spec: &GridSpec) -> Result<Vec<Cell>, FlowError> {
    let generated = generate_grid(area, spec).map_err(|e| FlowError::GridEngine(e.to_string()))?;
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
