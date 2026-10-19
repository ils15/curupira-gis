//! `ta-cli seed-demo` — projeto de demonstração com dados **sintéticos**.
//!
//! Cria (ou reutiliza) o projeto `demo` com o **schema real** da Fase 1:
//!
//! - `demo_aoi` — o polígono de `testdata/monitoring/aoi.geojson` (EPSG:4674),
//!   1 feature; o catálogo das classes do demo vai no JSONB de atributos dela
//!   (`attrs.classes`), já que a tabela `classes` chega na Fase 2;
//! - `demo_pontos` — os pontos de `testdata/monitoring/points_classes.csv`
//!   (CSV-XY, SRID declarado 4674) com o atributo `classe`;
//! - **AOI real** (`aoi_layers`) apontando `demo_aoi`;
//! - **grade real** (`cell_layers` + `cells`): resolvida por [`GRID_RESOLUTION`]
//!   graus com o motor puro [`ta_engine::generate_grid`] e persistida.
//!
//! Idempotência: cada camada é resolvida **por nome** dentro do projeto; o
//! vínculo de AOI é resolvido por camada; a grade é resolvida por nome e só é
//! (re)preenchida se estiver vazia. Rodar duas vezes não duplica nada.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use geo_types::MultiPolygon;
use serde_json::json;
use sqlx::PgPool;
use ta_core::aoi::AoiLayer;
use ta_core::cell::{DEFAULT_CELL_SRID, GridSpec};
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
use ta_gis::normalizer;
use ta_gis::reader::{CsvEncoding, CsvXyOptions, ReadOptions};

/// Camada da AOI (polígono do arquivo).
pub const AOI_LAYER: &str = "demo_aoi";
/// Camada dos pontos com atributo de classe.
pub const POINTS_LAYER: &str = "demo_pontos";
/// Nome da camada de células (grade real da Fase 1).
pub const GRID_NAME: &str = "demo_grade";
/// Resolução da grade do demo, **em graus** (EPSG:4674 é geográfico).
pub const GRID_RESOLUTION: f64 = 0.005;
/// Projeto do demo.
pub const DEMO_PROJECT: &str = "demo";

/// Argumentos do `seed-demo` (decodificados pelo clap em `main.rs`).
#[derive(Debug, Clone)]
pub struct SeedArgs {
    /// Projeto alvo (criado se não existir).
    pub project: String,
    /// Polígono da AOI (GeoJSON, EPSG:4674).
    pub aoi: PathBuf,
    /// Pontos com atributo de classe (CSV-XY).
    pub points: PathBuf,
    /// DSN do banco (senão env / default do compose).
    pub dsn: Option<String>,
}

/// Resultado de uma camada do seed.
struct Seeded {
    /// Id (UUID) da camada persistida.
    id: String,
    geom_type: GeomType,
    /// Features do lote (o que o seed representa).
    total: usize,
    /// Linhas efetivamente inseridas nesta execução.
    inserted: u64,
    /// `true` quando a camada já existia preenchida (nada foi feito).
    skipped: bool,
}

/// Resultado da grade real.
struct GridOutcome {
    layer_id: String,
    /// Total de células na camada após a execução.
    count: u64,
    /// Células inseridas nesta execução.
    inserted: u64,
    /// `true` quando a grade já existia preenchida.
    skipped: bool,
}

/// Porta de entrada do subcomando. Erros chegam como texto legível.
pub async fn run(args: SeedArgs) -> Result<(), String> {
    // Lê e normaliza as fontes ANTES de tocar no banco: arquivo ausente/CRS
    // ilegível falha cedo, sem deixar projeto meio criado.
    let (points_data, _) = normalizer::prepare_import(
        &args.points,
        &ReadOptions {
            srid: Some(normalizer::TARGET_SRID),
            csv: CsvXyOptions {
                srid: Some(normalizer::TARGET_SRID),
                encoding: CsvEncoding::Utf8,
                ..Default::default()
            },
        },
    )
    .map_err(|e| format!("lendo {}: {e}", args.points.display()))?;
    if points_data.features.is_empty() {
        return Err(format!("{}: sem linhas de dados", args.points.display()));
    }
    let classes = classes_of(&points_data.features);

    let (mut aoi_data, _) = normalizer::prepare_import(&args.aoi, &ReadOptions::default())
        .map_err(|e| format!("lendo {}: {e}", args.aoi.display()))?;
    if aoi_data.features.is_empty() {
        return Err(format!("{}: sem geometria de AOI", args.aoi.display()));
    }
    // Catálogo das classes no JSONB da AOI (a tabela `classes` é da Fase 2).
    annotate_aoi(&mut aoi_data.features, &classes, &args.aoi);
    let area = ta_flow::grid::multipolygon_from_features(&aoi_data.features)
        .map_err(|e| format!("{}: {e}", args.aoi.display()))?;

    let pool = crate::open_pool(args.dsn.as_deref()).await?;
    let projects = PgProjectRepo::new(pool.clone());
    let layers = PgLayerRepo::new(pool.clone());
    let feature_repo = PgFeatureRepo::new(pool.clone());

    let project = crate::resolve_project(&projects, &args.project).await?;
    println!("seed-demo: projeto `{}` ({})", project.name, project.id);

    let aoi = ensure_layer(
        &layers,
        &feature_repo,
        &project.id,
        AOI_LAYER,
        aoi_data.features,
        canonical(&args.aoi),
    )
    .await?;
    let points = ensure_layer(
        &layers,
        &feature_repo,
        &project.id,
        POINTS_LAYER,
        points_data.features,
        canonical(&args.points),
    )
    .await?;

    let aoi_link = ensure_aoi_layer(&pool, &project.id, &aoi.id, None).await?;
    let grid = ensure_grid(&pool, &project.id, &aoi_link.id, &area).await?;

    print_report(
        &project.name,
        &aoi,
        &points,
        &aoi_link,
        &grid,
        &classes,
        &args.aoi,
    );
    pool.close().await;
    Ok(())
}

/// Cria a camada e insere as features, ou reusa a camada existente.
///
/// Reuso por nome (`UNIQUE(project_id, name)`): com features já presentes,
/// nada é inserido (idempotência); vazia, é preenchida.
async fn ensure_layer(
    layers: &PgLayerRepo,
    feature_repo: &PgFeatureRepo,
    project_id: &str,
    name: &str,
    feats: Vec<Feature>,
    source_uri: String,
) -> Result<Seeded, String> {
    let total = feats.len();
    let geom_type = normalizer::dominant_geom_type(&feats).ok_or("lote sem geometrias")?;
    let existing = layers
        .list_for_project(project_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|l| l.name == name);

    match existing {
        Some(layer) => {
            let n = feature_repo
                .count(&layer.id)
                .await
                .map_err(|e| format!("contando `{name}`: {e}"))?;
            let (inserted, skipped) = if n > 0 {
                (0, true)
            } else {
                (
                    insert_and_bbox(feature_repo, layers, &layer.id, name, feats).await?,
                    false,
                )
            };
            Ok(Seeded {
                id: layer.id,
                geom_type,
                total,
                inserted,
                skipped,
            })
        }
        None => {
            let layer = layers
                .add(&Layer {
                    id: String::new(),
                    project_id: project_id.to_owned(),
                    name: name.to_owned(),
                    geom_type,
                    srid: normalizer::TARGET_SRID,
                    source_uri,
                })
                .await
                .map_err(|e| format!("criando camada `{name}`: {e}"))?;
            let inserted = insert_and_bbox(feature_repo, layers, &layer.id, name, feats).await?;
            Ok(Seeded {
                id: layer.id,
                geom_type,
                total,
                inserted,
                skipped: false,
            })
        }
    }
}

async fn insert_and_bbox(
    feature_repo: &PgFeatureRepo,
    layers: &PgLayerRepo,
    layer_id: &str,
    name: &str,
    feats: Vec<Feature>,
) -> Result<u64, String> {
    let inserted = feature_repo
        .bulk_insert(layer_id, feats)
        .await
        .map_err(|e| format!("bulk_insert em `{name}`: {e}"))?;
    let _ = layers
        .update_bbox(layer_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(inserted)
}

/// Resolve (ou cria) o vínculo de AOI do projeto para a camada dada.
async fn ensure_aoi_layer(
    pool: &PgPool,
    project_id: &str,
    layer_id: &str,
    id_column: Option<String>,
) -> Result<AoiLayer, String> {
    let repo = PgAoiLayerRepo::new(pool.clone());
    if let Some(existing) = repo
        .list_for_project(project_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|a| a.layer_id == layer_id)
    {
        return Ok(existing);
    }
    repo.create(&AoiLayer::new(project_id, layer_id, id_column))
        .await
        .map_err(|e| format!("criando vínculo de AOI: {e}"))
}

/// Resolve (ou cria) a grade real do demo e garante que ela tenha células.
async fn ensure_grid(
    pool: &PgPool,
    project_id: &str,
    aoi_layer_id: &str,
    area: &MultiPolygon<f64>,
) -> Result<GridOutcome, String> {
    let [origin_x, origin_y] = ta_engine::aoi_bbox(area)
        .map(|b| [b[0], b[1]])
        .ok_or("AOI sem envelope para alinhar a grade")?;
    let spec = GridSpec::new(GRID_RESOLUTION, origin_x, origin_y, DEFAULT_CELL_SRID);

    let cell_layers = PgCellLayerRepo::new(pool.clone());
    let cells = PgCellRepo::new(pool.clone());

    if let Some(existing) = cell_layers
        .list_for_project(project_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|c| c.name == GRID_NAME)
    {
        let n = cells
            .count(&existing.id)
            .await
            .map_err(|e| format!("contando células: {e}"))?;
        if n > 0 {
            return Ok(GridOutcome {
                layer_id: existing.id,
                count: n,
                inserted: 0,
                skipped: true,
            });
        }
        // Mesma política de projeto do `grid create` (o caminho do seed incluído).
        ta_flow::grid::ensure_max_cells(pool, project_id, area, &spec)
            .await
            .map_err(|e| e.to_string())?;
        let generated = ta_flow::grid::generate_cells(area, &spec).map_err(|e| e.to_string())?;
        let inserted = cells
            .bulk_insert(&existing.id, &generated, spec.srid)
            .await
            .map_err(|e| format!("gravando células: {e}"))?;
        let total = cells.count(&existing.id).await.map_err(|e| e.to_string())?;
        return Ok(GridOutcome {
            layer_id: existing.id,
            count: total,
            inserted,
            skipped: false,
        });
    }

    let (layer, inserted) =
        ta_flow::grid::persist_grid(pool, project_id, aoi_layer_id, GRID_NAME, &spec, area, None)
            .await
            .map_err(|e| e.to_string())?;
    // `inserted` pode ser menor que o total (`ON CONFLICT DO NOTHING`); o
    // relatório mostra o que **existe** na camada após a execução.
    let count = cells.count(&layer.id).await.map_err(|e| e.to_string())?;
    Ok(GridOutcome {
        layer_id: layer.id,
        count,
        inserted,
        skipped: false,
    })
}

/// Anota os atributos da AOI: classes do demo + origem do arquivo.
fn annotate_aoi(features: &mut [Feature], classes: &BTreeSet<String>, path: &Path) {
    let names: Vec<&str> = classes.iter().map(String::as_str).collect();
    for feature in features.iter_mut() {
        if let serde_json::Value::Object(attrs) = &mut feature.attrs {
            attrs.insert("classes".to_owned(), json!(names));
            attrs.insert("fonte".to_owned(), json!(path.display().to_string()));
        }
    }
}

/// Classes distintas do atributo `classe` (catálogo do demo).
#[must_use]
pub(crate) fn classes_of(features: &[Feature]) -> BTreeSet<String> {
    features
        .iter()
        .filter_map(|f| f.attrs.get("classe").and_then(|v| v.as_str()))
        .map(str::to_owned)
        .collect()
}

fn print_report(
    project: &str,
    aoi: &Seeded,
    points: &Seeded,
    aoi_link: &AoiLayer,
    grid: &GridOutcome,
    classes: &BTreeSet<String>,
    aoi_path: &Path,
) {
    println!("  {:<14} {:<10} {:>6}  estado", "camada", "tipo", "n");
    for (name, layer) in [(AOI_LAYER, aoi), (POINTS_LAYER, points)] {
        let state = if layer.skipped {
            "já existia (nada inserido)".to_owned()
        } else {
            format!("novo ({} inseridas)", layer.inserted)
        };
        println!(
            "  {:<14} {:<10} {:>6}  {}",
            name,
            format!("{:?}", layer.geom_type).to_lowercase(),
            layer.total,
            state,
        );
    }
    let grid_state = if grid.skipped {
        "já existia (nada inserido)".to_owned()
    } else {
        format!(
            "grade real ({} células inseridas, resolução {GRID_RESOLUTION} graus)",
            grid.inserted
        )
    };
    println!(
        "  {:<14} {:<10} {:>6}  {}",
        GRID_NAME, "cells", grid.count, grid_state
    );
    println!(
        "  classes ({}): {}",
        classes.len(),
        if classes.is_empty() {
            "—".to_owned()
        } else {
            classes.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    );
    println!(
        "  aoi: {}  (aoi_layers {})",
        aoi_path.display(),
        aoi_link.id
    );
    println!("  grade: cell_layer {}", grid.layer_id);
    println!("seed-demo ok — conecte a GUI no banco e abra o projeto `{project}`.");
}

/// Caminho canônico para `source_uri` (mesma convenção do import-vector).
fn canonical(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point_with_class(id: i64, class: &str) -> Feature {
        Feature::new(
            geo_types::Point::new(-52.2, -3.2).into(),
            json!({"id": id, "classe": class}),
        )
    }

    #[test]
    fn classes_distintas_sao_o_catalogo() {
        let features = vec![
            point_with_class(1, "floresta"),
            point_with_class(2, "desmatamento"),
            point_with_class(3, "floresta"),
            Feature::new(geo_types::Point::new(0.0, 0.0).into(), json!({})),
        ];
        let classes = classes_of(&features);
        assert_eq!(classes.len(), 2);
        assert!(classes.contains("floresta") && classes.contains("desmatamento"));
    }

    #[test]
    fn anotacao_da_aoi_grava_o_catalogo_de_classes() {
        let mut features = vec![Feature::new(
            geo_types::Point::new(-52.2, -3.2).into(),
            json!({"nome": "AOI"}),
        )];
        let classes: BTreeSet<String> = ["floresta", "hidrografia"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        annotate_aoi(&mut features, &classes, Path::new("/tmp/aoi.geojson"));
        assert_eq!(
            features[0].attrs["classes"],
            json!(["floresta", "hidrografia"])
        );
        assert_eq!(features[0].attrs["nome"], json!("AOI"));
    }
}
