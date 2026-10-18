//! `ta-cli seed-demo` — projeto de demonstração com dados **sintéticos**.
//!
//! Cria (ou reutiliza) o projeto `demo` com três camadas, usando apenas o
//! schema atual (`projects`, `layers`, `features`):
//!
//! - `demo_aoi` — o polígono de `testdata/monitoring/aoi.geojson` (EPSG:4674),
//!   1 feature; o catálogo das classes do demo vai no JSONB de atributos dela
//!   (`attrs.classes`), já que a tabela `classes` só chega na Fase 1;
//! - `demo_pontos` — os pontos de `testdata/monitoring/points_classes.csv`
//!   (CSV-XY, SRID declarado 4674) com o atributo `classe`;
//! - `demo_grade` — **PROVISÓRIO**: grade 2×2 derivada do bbox da AOI.
//!
//! ## Por que a grade é provisória
//!
//! As tabelas `cells`/`aoi_layers`/`cell_layers` são da Fase 1. Até lá a grade
//! vive como features (um polígono por célula, com `attrs.provisorio = true`),
//! o que já dá conteúdo ao mapa sem inventar schema. Quando a Fase 1 entrar,
//! [`seed_provisional_grid`] sai e o seed passa a escrever nas tabelas reais.
//!
//! Idempotência: cada camada é resolvida **por nome** dentro do projeto; se já
//! existe e tem features, nada é inserido de novo (rodar duas vezes não
//! duplica). Camada existente porém vazia (execução interrompida) é
//! preenchida.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use geo_types::{Geometry, LineString, Polygon};
use serde_json::json;
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo};
use ta_gis::normalizer;
use ta_gis::reader::{CsvEncoding, CsvXyOptions, ReadOptions};

/// Camada da AOI (polígono do arquivo).
pub const AOI_LAYER: &str = "demo_aoi";
/// Camada dos pontos com atributo de classe.
pub const POINTS_LAYER: &str = "demo_pontos";
/// Camada da grade — **provisória** (Fase 1 substitui pela tabela `cells`).
pub const GRID_LAYER: &str = "demo_grade";
/// Projeto do demo.
pub const DEMO_PROJECT: &str = "demo";

/// Células da grade provisória: 2 linhas × 2 colunas.
const GRID_ROWS: usize = 2;
const GRID_COLS: usize = 2;

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
    geom_type: GeomType,
    /// Features do lote (o que o seed representa).
    total: usize,
    /// Linhas efetivamente inseridas nesta execução.
    inserted: u64,
    /// `true` quando a camada já existia preenchida (nada foi feito).
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
    let aoi_bbox = normalizer::bbox_of(&aoi_data.features).ok_or_else(|| {
        format!(
            "{}: não consegui calcular o bbox da AOI",
            args.aoi.display()
        )
    })?;

    // Catálogo das classes no JSONB da AOI (schema atual não tem tabela de
    // classes — ver doc do módulo).
    annotate_aoi(&mut aoi_data.features, &classes, &args.aoi);

    // PROVISÓRIO (Fase 0 → Fase 1): grade mínima como features; cada célula
    // leva `attrs.provisorio = true`.
    let grid_features = seed_provisional_grid(aoi_bbox)?;

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
    let grid = ensure_layer(
        &layers,
        &feature_repo,
        &project.id,
        GRID_LAYER,
        grid_features,
        format!("seed://{}/{}", project.name, GRID_LAYER),
    )
    .await?;

    print_report(
        &project.name,
        &[
            (AOI_LAYER, &aoi),
            (POINTS_LAYER, &points),
            (GRID_LAYER, &grid),
        ],
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

    let (inserted, skipped) = match existing {
        Some(layer) => {
            let n = feature_repo
                .count(&layer.id)
                .await
                .map_err(|e| format!("contando `{name}`: {e}"))?;
            if n > 0 {
                (0, true)
            } else {
                (
                    insert_and_bbox(feature_repo, layers, &layer.id, name, feats).await?,
                    false,
                )
            }
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
            (
                insert_and_bbox(feature_repo, layers, &layer.id, name, feats).await?,
                false,
            )
        }
    };

    Ok(Seeded {
        geom_type,
        total,
        inserted,
        skipped,
    })
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

/// **PROVISÓRIO** — grade `GRID_ROWS × GRID_COLS` sobre o bbox da AOI.
///
/// Um polígono por célula, com `attrs = {cell_id, row, col, provisorio}`.
/// A Fase 1 substitui isto pela tabela `cells` (AOI/célula/camada).
fn seed_provisional_grid(bbox: [f64; 4]) -> Result<Vec<Feature>, String> {
    let [min_x, min_y, max_x, max_y] = bbox;
    let width = max_x - min_x;
    let height = max_y - min_y;
    if !(width > 0.0 && height > 0.0) {
        return Err(format!(
            "bbox degenerado para a grade provisória: [{min_x}, {min_y}, {max_x}, {max_y}]"
        ));
    }
    let step_x = width / GRID_COLS as f64;
    let step_y = height / GRID_ROWS as f64;

    let mut cells = Vec::with_capacity(GRID_ROWS * GRID_COLS);
    for row in 0..GRID_ROWS {
        for col in 0..GRID_COLS {
            let x0 = min_x + step_x * col as f64;
            let y0 = min_y + step_y * row as f64;
            cells.push(Feature::new(
                cell_polygon(x0, y0, x0 + step_x, y0 + step_y),
                json!({
                    "cell_id": format!("r{row}c{col}"),
                    "row": row,
                    "col": col,
                    "provisorio": true,
                }),
            ));
        }
    }
    Ok(cells)
}

/// Polígono fechado de uma célula (anel externo, sem buracos).
fn cell_polygon(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Geometry<f64> {
    let ring = LineString::from(vec![
        (min_x, min_y),
        (max_x, min_y),
        (max_x, max_y),
        (min_x, max_y),
        (min_x, min_y),
    ]);
    Polygon::new(ring, Vec::new()).into()
}

fn print_report(project: &str, seeded: &[(&str, &Seeded)], classes: &BTreeSet<String>, aoi: &Path) {
    println!("  {:<14} {:<10} {:>6}  estado", "camada", "tipo", "n");
    for (name, layer) in seeded {
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
    println!(
        "  classes ({}): {}",
        classes.len(),
        if classes.is_empty() {
            "—".to_owned()
        } else {
            classes.iter().cloned().collect::<Vec<_>>().join(", ")
        }
    );
    println!("  aoi: {}", aoi.display());
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
    use serde_json::Value;

    fn point_with_class(id: i64, class: &str) -> Feature {
        Feature::new(
            geo_types::Point::new(-52.2, -3.2).into(),
            json!({"id": id, "classe": class}),
        )
    }

    #[test]
    fn grade_provisoria_cobre_o_bbox_em_2x2() {
        let cells = seed_provisional_grid([-52.20, -3.22, -52.14, -3.16]).expect("grade");
        assert_eq!(cells.len(), 4, "2 linhas × 2 colunas");

        let bbox = normalizer::bbox_of(&cells).expect("bbox das células");
        assert!((bbox[0] - -52.20).abs() < 1e-12 && (bbox[2] - -52.14).abs() < 1e-12);
        assert!((bbox[1] - -3.22).abs() < 1e-12 && (bbox[3] - -3.16).abs() < 1e-12);

        let ids: Vec<String> = cells
            .iter()
            .map(|c| c.attrs["cell_id"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(ids, vec!["r0c0", "r0c1", "r1c0", "r1c1"]);
        assert!(
            cells
                .iter()
                .all(|c| c.attrs["provisorio"] == Value::Bool(true))
        );
    }

    #[test]
    fn bbox_degenerado_e_erro_legivel() {
        let err = seed_provisional_grid([-52.0, -3.0, -52.0, -3.0]).unwrap_err();
        assert!(err.contains("degenerado"), "erro: {err}");
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
