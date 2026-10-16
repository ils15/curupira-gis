//! Implementação dos subcomandos `import-vector`, `import-raster` e
//! `export` — pipeline ta-gis (detect → read → normalize → insert) +
//! repos ta-db.

use std::path::{Path, PathBuf};
use std::time::Instant;

use sqlx::PgPool;
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo};
use ta_gis::normalizer;
use ta_gis::reader::{CsvEncoding, CsvXyOptions, ReadOptions};

use crate::resolve_project;

/// Argumentos do `import-vector` (decodificados pelo clap em main.rs).
pub(crate) struct ImportVectorArgs {
    pub input: PathBuf,
    pub project: Option<String>,
    pub layer_name: Option<String>,
    pub srid: Option<u32>,
    pub lon_col: Option<String>,
    pub lat_col: Option<String>,
    pub sep: Option<char>,
    pub encoding: CsvEncoding,
    pub dsn: Option<String>,
}

/// Argumentos do `import-raster`.
pub(crate) struct ImportRasterArgs {
    pub input: PathBuf,
    pub project: Option<String>,
    pub layer_name: Option<String>,
    /// Produz COG derivado ao lado antes de registrar.
    pub cog: Option<PathBuf>,
    pub dsn: Option<String>,
}

/// Argumentos do `export`.
pub(crate) struct ExportArgs {
    pub layer: String,
    pub format: String,
    pub output: PathBuf,
    pub srid_out: u32,
    pub dsn: Option<String>,
}

pub(crate) async fn import_vector(args: ImportVectorArgs) -> Result<(), String> {
    let started = Instant::now();
    let path: &Path = &args.input;
    let opts = ReadOptions {
        srid: args.srid,
        csv: CsvXyOptions {
            lon_col: args.lon_col.clone(),
            lat_col: args.lat_col.clone(),
            sep: args.sep,
            srid: args.srid,
            encoding: args.encoding,
        },
    };

    // 1) detecta → lê → normaliza (valida + reprojeção p/ 4674).
    let (data, report) = normalizer::prepare_import(path, &opts)
        .map_err(|e| format!("import de {}: {e}", path.display()))?;
    if data.features.is_empty() {
        return Err(format!(
            "{}: nenhum registro vetorial — nada a importar",
            path.display()
        ));
    }

    // 2) banco: projeto/camada/insert/bbox.
    let pool = crate::open_pool(args.dsn.as_deref()).await?;
    let (_project, layer) =
        prepare_project_layer(&pool, &args.project, path, &args.layer_name, &data, &report).await?;

    let features: Vec<Feature> = data.features.clone();
    let n = features.len();
    let feature_repo = PgFeatureRepo::new(pool.clone());
    let inserted = feature_repo
        .bulk_insert(&layer.id, features)
        .await
        .map_err(|e| format!("bulk_insert na camada {}: {e}", layer.name))?;
    let _ = PgLayerRepo::new(pool.clone())
        .update_bbox(&layer.id)
        .await
        .map_err(|e| e.to_string())?;

    let bbox = normalizer::bbox_of(&data.features);
    println!(
        "import-vector ok: {} → camada `{}` ({}) no banco\n  features: {n} (inseridas {inserted})\n  srid: {:?} → 4674 (reprojetado: {})\n  bbox: {}\n  tempo: {} ms",
        path.display(),
        layer.name,
        layer.id,
        report.input_srid,
        report.reprojected,
        bbox.map(|b| format!("[{:.6}, {:.6}, {:.6}, {:.6}]", b[0], b[1], b[2], b[3]))
            .unwrap_or_else(|| "—".into()),
        started.elapsed().as_millis(),
    );
    pool.close().await;
    Ok(())
}

async fn prepare_project_layer(
    pool: &PgPool,
    project: &Option<String>,
    path: &Path,
    layer_name: &Option<String>,
    data: &ta_gis::reader::VectorData,
    report: &normalizer::NormalizationReport,
) -> Result<(ta_core::project::Project, Layer), String> {
    use ta_db::repo::PgProjectRepo;
    let projects = PgProjectRepo::new(pool.clone());
    let key = project.clone().unwrap_or_else(|| "default".into());
    let proj = resolve_project(&projects, &key).await?;

    let base_name = layer_name
        .clone()
        .unwrap_or_else(|| data.layer_name.clone());
    let layers = PgLayerRepo::new(pool.clone());
    let existing: Vec<String> = layers
        .list_for_project(&proj.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|l| l.name)
        .collect();
    let name = unique_layer_name(&base_name, &existing);

    let geom_type = normalizer::dominant_geom_type(&data.features).ok_or("lote sem geometrias")?;
    let layer = layers
        .add(&Layer {
            id: String::new(),
            project_id: proj.id.clone(),
            name: name.clone(),
            geom_type,
            srid: normalizer::TARGET_SRID,
            source_uri: canonical(path),
        })
        .await
        .map_err(|e| format!("criar camada `{name}`: {e}"))?;
    let _ = report;
    Ok((proj, layer))
}

/// Caminho canônico (absoluto) para `source_uri`.
fn canonical(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// `nome`, `nome_2`, `nome_3`… evita colisão com UNIQUE(project, name).
fn unique_layer_name(base: &str, existing: &[String]) -> String {
    if !existing.iter().any(|e| e == base) {
        return base.to_string();
    }
    for i in 2..10_000 {
        let candidate = format!("{base}_{i}");
        if !existing.iter().any(|e| e == &candidate) {
            return candidate;
        }
    }
    format!("{base}_{}", std::process::id())
}

pub(crate) async fn import_raster(args: ImportRasterArgs) -> Result<(), String> {
    import_raster_impl(args).await
}

#[cfg(feature = "gdal")]
async fn import_raster_impl(args: ImportRasterArgs) -> Result<(), String> {
    let started = Instant::now();
    let path = &args.input;

    // COG opcional ANTES do registro — o source_uri aponta o derivado.
    let source = match &args.cog {
        Some(out) => ta_gis::derived::translate_cog(path, out, &[])
            .map_err(|e| format!("gdal_translate COG: {e}"))?,
        None => path.to_path_buf(),
    };

    let meta = ta_gis::reader::read_raster_meta(&source)
        .map_err(|e| format!("metadados de {}: {e}", source.display()))?;

    let pool = crate::open_pool(args.dsn.as_deref()).await?;
    let (proj, layer) =
        raster_project_layer(&pool, &args.project, &source, &args.layer_name, &meta).await?;

    println!(
        "import-raster ok: {} → camada `{}` ({})\n  {}×{} px, {} banda(s), block {}×{}\n  nodata: {:?}\n  srid: {:?} → 4674\n  bbox: [{:.6}, {:.6}, {:.6}, {:.6}]\n  source_uri: {}\n  tempo: {} ms\n  (pixels ficam no arquivo — nada copiado para o PG)",
        source.display(),
        layer.name,
        layer.id,
        meta.width,
        meta.height,
        meta.bands,
        meta.block_size[0],
        meta.block_size[1],
        meta.nodata,
        meta.srid,
        meta.bbox[0],
        meta.bbox[1],
        meta.bbox[2],
        meta.bbox[3],
        layer.source_uri,
        started.elapsed().as_millis(),
    );
    println!("  projeto: {} ({})", proj.name, proj.id);
    pool.close().await;
    Ok(())
}

#[cfg(not(feature = "gdal"))]
async fn import_raster_impl(_args: ImportRasterArgs) -> Result<(), String> {
    Err(
        "import-raster requer a feature `gdal` do ta-cli: recompile com \
         `cargo build -p ta-cli --features gdal`"
            .into(),
    )
}

/// Camada raster: geom_type=Raster, srid 4674, bbox do transform (reprojetado
/// quando o raster não está em 4674).
#[cfg(feature = "gdal")]
async fn raster_project_layer(
    pool: &PgPool,
    project: &Option<String>,
    source: &Path,
    layer_name: &Option<String>,
    meta: &ta_gis::raster::RasterMeta,
) -> Result<(ta_core::project::Project, Layer), String> {
    use ta_db::repo::PgProjectRepo;
    let projects = PgProjectRepo::new(pool.clone());
    let key = project.clone().unwrap_or_else(|| "default".into());
    let proj = resolve_project(&projects, &key).await?;

    let bbox_4674 = bbox_em_4674(meta)?;

    let base = layer_name.clone().unwrap_or_else(|| {
        source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("raster")
            .into()
    });
    let layers = PgLayerRepo::new(pool.clone());
    let existing: Vec<String> = layers
        .list_for_project(&proj.id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|l| l.name)
        .collect();
    let name = unique_layer_name(&base, &existing);

    let layer = layers
        .add(&Layer {
            id: String::new(),
            project_id: proj.id.clone(),
            name: name.clone(),
            geom_type: GeomType::Raster,
            srid: normalizer::TARGET_SRID,
            source_uri: canonical(source),
        })
        .await
        .map_err(|e| format!("criar camada raster `{name}`: {e}"))?;
    layers
        .set_bbox(
            &layer.id,
            bbox_4674[0],
            bbox_4674[1],
            bbox_4674[2],
            bbox_4674[3],
        )
        .await
        .map_err(|e| format!("gravar bbox da camada raster: {e}"))?;
    Ok((proj, layer))
}

/// Bbox do raster em 4674 — reprojeta os 4 cantos quando o CRS do arquivo
/// não é 4674 (feature gdal: OGR/PROJ).
#[cfg(feature = "gdal")]
fn bbox_em_4674(meta: &ta_gis::raster::RasterMeta) -> Result<[f64; 4], String> {
    if meta.srid.is_some_and(|s| s != normalizer::TARGET_SRID) {
        let srid = meta.srid.unwrap_or_default();
        let tr = ta_gis::ogr::make_transform(srid, normalizer::TARGET_SRID)
            .map_err(|e| format!("transformar bbox do raster {srid}→4674: {e}"))?;
        let mut xs = [meta.bbox[0], meta.bbox[2]];
        let mut ys = [meta.bbox[1], meta.bbox[3]];
        let mut zs = [0.0, 0.0];
        tr.transform_coords(&mut xs, &mut ys, &mut zs)
            .map_err(|e| format!("transformar cantos do raster: {e}"))?;
        return Ok([xs[0], ys[0], xs[1], ys[1]]);
    }
    Ok(meta.bbox)
}

pub(crate) async fn export_layer(args: ExportArgs) -> Result<(), String> {
    export_layer_impl(args).await
}

#[cfg(feature = "db")]
async fn export_layer_impl(args: ExportArgs) -> Result<(), String> {
    use ta_gis::writer::{self, WriteFormat, WriteOptions};

    let format = WriteFormat::parse(&args.format).ok_or_else(|| {
        format!(
            "formato inválido `{}` (shp|geojson|gpkg|fgb|csv)",
            args.format
        )
    })?;
    let started = Instant::now();

    let pool = crate::open_pool(args.dsn.as_deref()).await?;
    let layers = PgLayerRepo::new(pool.clone());
    let layer = layers
        .get(&args.layer)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("camada {} não encontrada", args.layer))?;

    let features_repo = PgFeatureRepo::new(pool.clone());
    let count = writer::export(
        &features_repo,
        &layer,
        format,
        &args.output,
        &WriteOptions {
            target_srid: args.srid_out,
        },
    )
    .await
    .map_err(|e| format!("export {}: {e}", format.name()))?;

    println!(
        "export ok: camada `{}` → {} ({} features) em {}\n  tempo: {} ms",
        layer.name,
        format.name(),
        count,
        args.output.display(),
        started.elapsed().as_millis(),
    );
    pool.close().await;
    Ok(())
}

#[cfg(not(feature = "db"))]
async fn export_layer_impl(_args: ExportArgs) -> Result<(), String> {
    Err("export requer a feature `db` do ta-cli: recompile com \
         `cargo build -p ta-cli --features db`"
        .into())
}
