//! Bench de decisão da Fase 3 — render headless de mapa (egui, sem GPU).
//!
//! Cenários do DISCOVERY.md (dados gerados por `scripts/gen_bench_fixtures.py`,
//! seed 42, NÃO commitados — apenas o script e o manifest):
//! 1. 10k polígonos (SHP EPSG:4674);
//! 2. 50k pontos (CSV-XY EPSG:4674);
//! 3. ambos + raster 1024² (GeoTIFF, 4 bandas UInt16).
//!
//! Método: loop de frames via `egui::Context::run_pass` (o MESMO caminho do
//! app — `MapViewEgui::render`), medindo elapsed por frame com `Instant`;
//! memória via RSS do processo (`/proc/self/status`) + estimativa de bytes
//! das geometrias. Critério de escape: **p95 > 33 ms (< 30 fps) → PROPOSTA de
//! Fase 3b (Tauri+MapLibre)**, documentada em `benches/map_render.md`.
//!
//! Rodar: `cargo bench -p ta-gui` (perfil bench = opt-level 3) — precisa de
//! `testdata/bench/` gerado antes.

use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::style::Style;
use ta_gis::normalizer;
use ta_gui::map_view::{Bbox, LayerFeature, LayerHandle, MapView, MapViewEgui, RenderCtx};

const FRAME_W: f32 = 1280.0;
const FRAME_H: f32 = 800.0;
const WARMUP: usize = 5;
const FRAMES: usize = 120;

struct Scenario {
    name: &'static str,
    layers: Vec<LayerHandle>,
    raster: Option<(PathBuf, ta_gis::raster::RasterMeta)>,
    extent: [f64; 4],
}

type Res<T> = Result<T, ta_gis::Error>;

fn load_csv_points(path: &Path) -> Res<Vec<Feature>> {
    // CSV-XY bench é EPSG:4674 declarado (lon/lat) — ReadOptions com srid.
    let opts = ta_gis::reader::ReadOptions {
        srid: Some(4674),
        csv: ta_gis::reader::CsvXyOptions {
            srid: Some(4674),
            ..Default::default()
        },
    };
    let data = ta_gis::reader::read_vector(path, &opts)?;
    let (features, _report) =
        normalizer::normalize(data.features, Some(4674), normalizer::TARGET_SRID)?;
    Ok(features)
}

fn load_shp_polygons(path: &Path) -> Res<(Vec<Feature>, GeomType)> {
    let data = ta_gis::reader::read_vector(path, &ta_gis::reader::ReadOptions::default())?;
    let srid = data.srid;
    let (features, _report) = normalizer::normalize(data.features, srid, normalizer::TARGET_SRID)?;
    let geom = normalizer::dominant_geom_type(&features).unwrap_or(GeomType::Polygon);
    Ok((features, geom))
}

fn layer_of(name: &str, geom: GeomType, features: Vec<Feature>) -> LayerHandle {
    LayerHandle {
        layer: Layer {
            id: name.into(),
            project_id: "bench".into(),
            name: name.into(),
            geom_type: geom,
            srid: 4674,
            source_uri: String::new(),
        },
        visible: true,
        style: Style::default(),
        features: features.into_iter().map(LayerFeature::local).collect(),
    }
}

fn rss_kb() -> u64 {
    // Linux: VmRSS do próprio processo (bench = processo dedicado).
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
                return kb.parse().unwrap_or(0);
            }
        }
    }
    0
}

/// Estimativa de memória das features (coords f64 ×2 + attrs JSON serializado).
fn estimate_bytes(layers: &[LayerHandle]) -> u64 {
    let mut bytes = 0u64;
    for l in layers {
        for lf in &l.features {
            bytes += 16 * (crate_coords(&lf.feature.geom)) as u64;
            bytes += serde_json::to_vec(&lf.feature.attrs)
                .map(|v| v.len() as u64)
                .unwrap_or(0);
        }
    }
    bytes
}

fn crate_coords(geom: &geo_types::Geometry<f64>) -> usize {
    use geo_types::Geometry::*;
    match geom {
        Point(_) => 1,
        Line(_) => 2,
        LineString(ls) => ls.0.len(),
        Polygon(p) => p.exterior().0.len(),
        MultiPoint(mp) => mp.0.len(),
        MultiLineString(mls) => mls.0.iter().map(|l| l.0.len()).sum(),
        MultiPolygon(mp) => mp.0.iter().map(|p| p.exterior().0.len()).sum(),
        Rect(_) | Triangle(_) => 5,
        GeometryCollection(gc) => gc.0.iter().map(crate_coords).sum(),
    }
}

/// Roda `frames` frames headless medindo elapsed por frame (ms).
fn run_frames(view: &mut MapViewEgui, frames: usize, warmup: usize) -> (Vec<f64>, usize) {
    let ctx = egui::Context::default();
    ctx.tessellation_options_mut(|o| {
        o.feathering = true;
    });
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(FRAME_W, FRAME_H),
        )),
        ..Default::default()
    };
    let mut times = Vec::with_capacity(frames);
    let mut shapes_last = 0usize;
    for i in 0..(frames + warmup) {
        let t0 = Instant::now();
        let mut out = ctx.run_pass(raw.clone(), |ctx| {
            let mut rctx = RenderCtx {
                egui: ctx,
                origin_px: (0.0, 0.0),
                viewport_px: (FRAME_W, FRAME_H),
            };
            view.render(&mut rctx);
        });
        let ppp = out.pixels_per_point;
        let shapes = std::mem::take(&mut out.shapes);
        // Headless: deltas de textura (fonte) não têm backend — descarte
        // explícito via API oficial (evita o assert de Drop do egui).
        out.drop_without_applying_deltas();
        let prims = ctx.tessellate(shapes, ppp);
        shapes_last = prims.len();
        let dt = t0.elapsed().as_secs_f64() * 1000.0;
        if i >= warmup {
            times.push(dt);
        }
    }
    (times, shapes_last)
}

fn stats(times: &[f64]) -> (f64, f64, f64) {
    let mut sorted = times.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let n = sorted.len().max(1);
    let mean = sorted.iter().sum::<f64>() / n as f64;
    let p95 = sorted[((n as f64 * 0.95).ceil() as usize).min(n) - 1];
    (mean, p95, sorted[n - 1])
}

fn main() {
    let data_dir = std::env::var("TA_BENCH_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/bench"));
    println!("# dados: {}", data_dir.display());
    if !data_dir.exists() {
        eprintln!(
            "ERRO: {} ausente — gere com `python3 scripts/gen_bench_fixtures.py`",
            data_dir.display()
        );
        std::process::exit(2);
    }

    // --- Carga (fora do loop de frames) ---
    let t0 = Instant::now();
    let polys = load_shp_polygons(&data_dir.join("bench_polys.shp")).expect("ler SHP 10k");
    let points = load_csv_points(&data_dir.join("bench_points.csv")).expect("ler CSV 50k");
    let raster_meta = ta_gis::reader::read_raster_meta(&data_dir.join("bench_raster.tif"))
        .expect("meta do raster 1024²");
    println!(
        "# carga: {polys_n} polígonos + {points_n} pontos em {ms:.0} ms",
        polys_n = polys.0.len(),
        points_n = points.len(),
        ms = t0.elapsed().as_secs_f64() * 1000.0
    );

    let extent = [-73.9, -10.6, -49.6, 2.6];
    let fit = |view: &mut MapViewEgui, ext: [f64; 4]| {
        view.zoom_to(Bbox::new(ext[0], ext[1], ext[2], ext[3]));
    };

    let mut scenarios = vec![
        Scenario {
            name: "10k_poligonos",
            layers: vec![layer_of("polys", polys.1, polys.0.clone())],
            raster: None,
            extent,
        },
        Scenario {
            name: "50k_pontos",
            layers: vec![layer_of("points", GeomType::Point, points.clone())],
            raster: None,
            extent,
        },
        Scenario {
            name: "ambos_mais_raster_1024",
            layers: vec![
                layer_of("polys", polys.1, polys.0.clone()),
                layer_of("points", GeomType::Point, points.clone()),
            ],
            raster: Some((data_dir.join("bench_raster.tif"), raster_meta)),
            extent,
        },
    ];
    // Gatilho objetivo da 3b (DISCOVERY): raster grande. Só roda se o
    // arquivo existir (gerado com `--stress`).
    let stress_path = data_dir.join("bench_raster_stress.tif");
    if stress_path.exists() {
        if let Ok(stress_meta) = ta_gis::reader::read_raster_meta(&stress_path) {
            scenarios.push(Scenario {
                name: "raster_stress_8192",
                layers: vec![],
                raster: Some((stress_path, stress_meta)),
                extent,
            });
        }
    }

    let rss_before = rss_kb();
    let mut rows = Vec::new();
    for sc in &mut scenarios {
        let mut view = MapViewEgui::new();
        view.set_layers(sc.layers.clone());
        if let Some((path, meta)) = &sc.raster {
            match ta_gui::raster_view::load_band(path, 0) {
                Ok(img) => {
                    view.set_raster(Some(ta_gui::map_view::RasterLayerView {
                        path: path.clone(),
                        bbox: Bbox::new(meta.bbox[0], meta.bbox[1], meta.bbox[2], meta.bbox[3]),
                        band: 0,
                        band_count: meta.bands,
                        image: Some(img),
                    }));
                }
                Err(e) => eprintln!("aviso: raster não carregado ({e})"),
            }
        }
        fit(&mut view, sc.extent);
        let (times, prims) = run_frames(&mut view, FRAMES, WARMUP);
        let (mean, p95, max) = stats(&times);
        let mem = estimate_bytes(&sc.layers);
        rows.push(format!(
            "| {} | {:.2} | {:.2} | {:.2} | {:.1} MiB | {} |",
            sc.name,
            mean,
            p95,
            max,
            mem as f64 / (1024.0 * 1024.0),
            prims
        ));
        println!(
            "{}: mean {:.2} ms | p95 {:.2} ms | max {:.2} ms | prims {}",
            sc.name, mean, p95, max, prims
        );
    }
    let rss_after = rss_kb();
    println!(
        "# RSS do processo: antes {rss_before} KiB → depois {rss_after} KiB (Δ {} KiB)",
        rss_after as i64 - rss_before as i64
    );
    println!();
    println!("| cenário | mean (ms) | p95 (ms) | max (ms) | dados (est.) | prims/frame |");
    println!("|---|---|---|---|---|---|");
    for r in rows {
        println!("{r}");
    }
}
