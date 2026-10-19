//! Baseline de performance do `ta-db` (Fase 0) — harness `criterion`.
//!
//! Cenários, com o MESMO dataset sintético do teste gated
//! (`tests/pg_integration.rs`, 10k polígonos EPSG:4674):
//!
//! 1. `bulk_insert/10k_poligonos` — COPY FROM STDIN (fallback batched INSERT).
//!    O `DELETE` que limpa a camada entra no *setup* da iteração, fora da
//!    medição (senão a tabela cresceria sem limite e o número derivaria);
//! 2. `bbox_query/janela_viewport` — janela grande (`&&` + GIST);
//! 3. `list_with_ids/10k` — leitura completa com id de linha (tabela + undo);
//! 4. `mvt_tile/z12` — só quando o servidor tem `ST_AsMVT` com protobuf-c
//!    (a imagem `postgis/postgis:16-3.4` tem; builds conda-forge normalmente
//!    NÃO têm, e o cenário é anunciado como pulado).
//!
//! Gate: `TA_TEST_PG=1` + `TA_PG_DSN` (harness `scripts/test-pg.sh`). Sem o
//! gate, o bench imprime o motivo e sai 0 — não quebra `cargo bench` nem o
//! step informativo do CI.
//!
//!   TA_TEST_PG=1 TA_PG_DSN=postgres://ta:ta@127.0.0.1:55432/ta \
//!       cargo bench -p ta-db
//!
//! Números publicados: `benches/BASELINE.md`.

use std::hint::black_box;
use std::time::Duration;

use criterion::{BatchSize, Criterion};
use geo_types::{LineString, Polygon};
use sqlx::PgPool;
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::spatial::{
    MvtTile, TARGET_SRID, ensure_postgis, mvt_available, mvt_tile, run_migrations,
};
use tokio::runtime::Runtime;

/// DSN padrão = harness de `scripts/test-pg.sh`.
const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";
/// Tamanho do lote do baseline (o mesmo do teste gated de 10k).
const N: usize = 10_000;

struct Ctx {
    pool: PgPool,
    /// Camada estática com os 10k (bbox/list_with_ids).
    read_layer: String,
    /// Camada do bulk_insert, esvaziada a cada iteração.
    write_layer: String,
    /// `ST_AsMVT` utilizável neste servidor?
    mvt: bool,
}

/// Falha explícita e legível (sem panic) quando um passo do bench quebra.
fn must<T, E: std::fmt::Display>(result: Result<T, E>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(e) => {
            eprintln!("[bench ta-db] `{what}` falhou: {e}");
            std::process::exit(1);
        }
    }
}

/// Mesmo dataset do teste gated: 10k polígonos pequenos + 3 atributos.
fn dataset() -> Vec<Feature> {
    (0..N)
        .map(|i| {
            let cx = -73.0 + (i % 2300) as f64 * 0.01;
            let cy = -10.0 + (i / 2300) as f64 * 0.01;
            let classe = ["floresta", "pastagem", "agricultura"][i % 3];
            Feature::new(
                sample_polygon(cx, cy, 0.002).into(),
                serde_json::json!({
                    "id": i,
                    "nome": format!("poligonal_{i:05}"),
                    "classe": classe,
                }),
            )
        })
        .collect()
}

fn sample_polygon(cx: f64, cy: f64, r: f64) -> Polygon<f64> {
    Polygon::new(
        LineString::from(vec![
            (cx - r, cy - r),
            (cx + r, cy - r),
            (cx + r, cy + r),
            (cx - r, cy + r),
            (cx - r, cy - r),
        ]),
        vec![],
    )
}

async fn setup() -> Result<Ctx, String> {
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    let pool = PgPool::connect(&dsn)
        .await
        .map_err(|e| format!("conectar em {dsn}: {e}"))?;
    ensure_postgis(&pool).await.map_err(|e| e.to_string())?;
    run_migrations(&pool).await.map_err(|e| e.to_string())?;

    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: format!("bench_{stamp}"),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .map_err(|e| format!("criar projeto: {e}"))?;

    let layers = PgLayerRepo::new(pool.clone());
    let mut ids = Vec::new();
    for name in ["bench_read", "bench_write"] {
        let layer = layers
            .add(&Layer {
                id: String::new(),
                project_id: project.id.clone(),
                name: name.to_owned(),
                geom_type: GeomType::Polygon,
                srid: TARGET_SRID,
                source_uri: "bench://ta-db".to_owned(),
            })
            .await
            .map_err(|e| format!("criar camada {name}: {e}"))?;
        ids.push(layer.id);
    }

    let features = PgFeatureRepo::new(pool.clone());
    let inserted = features
        .bulk_insert(&ids[0], dataset())
        .await
        .map_err(|e| format!("carga inicial de {N}: {e}"))?;
    eprintln!("[bench ta-db] carga inicial do cenário de leitura: {inserted} features");

    let mvt = mvt_available(&pool).await.unwrap_or(false);
    Ok(Ctx {
        pool,
        read_layer: ids[0].clone(),
        write_layer: ids[1].clone(),
        mvt,
    })
}

fn main() {
    if std::env::var("TA_TEST_PG").ok().as_deref() != Some("1") {
        eprintln!(
            "[bench ta-db] skipped: defina TA_TEST_PG=1 e TA_PG_DSN \
             (harness: `bash scripts/test-pg.sh --no-tests`)"
        );
        return;
    }
    let rt = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[bench ta-db] runtime tokio: {e}");
            std::process::exit(1);
        }
    };
    let ctx = match rt.block_on(setup()) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("[bench ta-db] setup falhou: {e}");
            std::process::exit(1);
        }
    };

    let mut c = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .configure_from_args();

    bench_bulk_insert(&mut c, &ctx, &rt);
    bench_bbox_query(&mut c, &ctx, &rt);
    bench_list_with_ids(&mut c, &ctx, &rt);
    if ctx.mvt {
        bench_mvt_tile(&mut c, &ctx, &rt);
    } else {
        eprintln!(
            "[bench ta-db] mvt_tile pulado: este PostGIS não tem ST_AsMVT com protobuf-c \
             (use a imagem postgis/postgis:16-3.4 para medir)"
        );
    }

    c.final_summary();
    rt.block_on(ctx.pool.close());
}

fn bench_bulk_insert(c: &mut Criterion, ctx: &Ctx, rt: &Runtime) {
    let repo = PgFeatureRepo::new(ctx.pool.clone());
    let data = dataset();
    let mut group = c.benchmark_group("bulk_insert");
    group.sample_size(10);
    group.bench_function(format!("{}k_poligonos", N / 1000), |b| {
        b.iter_batched(
            // Setup (fora da medição): esvazia a camada de escrita.
            || {
                let pool = &ctx.pool;
                let layer = &ctx.write_layer;
                must(
                    rt.block_on(async {
                        sqlx::query("DELETE FROM features WHERE layer_id = $1::uuid")
                            .bind(layer)
                            .execute(pool)
                            .await
                            .map(|_| ())
                    }),
                    "limpar camada de escrita",
                );
            },
            |()| {
                let inserted = must(
                    rt.block_on(repo.bulk_insert(&ctx.write_layer, data.clone())),
                    "bulk_insert",
                );
                black_box(inserted)
            },
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

fn bench_bbox_query(c: &mut Criterion, ctx: &Ctx, rt: &Runtime) {
    let repo = PgFeatureRepo::new(ctx.pool.clone());
    let mut group = c.benchmark_group("bbox_query");
    // Janela de "viewport": 10° de longitude × 4° de latitude (≈ 4k features
    // dos 10k) — o caso de render, não o de pick.
    group.bench_function("janela_viewport", |b| {
        b.iter(|| {
            let rows = must(
                rt.block_on(repo.bbox_query(&ctx.read_layer, -60.0, -12.0, -50.0, -8.0)),
                "bbox_query",
            );
            black_box(rows.len())
        });
    });
    group.finish();
}

fn bench_list_with_ids(c: &mut Criterion, ctx: &Ctx, rt: &Runtime) {
    let repo = PgFeatureRepo::new(ctx.pool.clone());
    let mut group = c.benchmark_group("list_with_ids");
    group.bench_function(format!("{}k", N / 1000), |b| {
        b.iter(|| {
            let rows = must(
                rt.block_on(repo.list_with_ids(&ctx.read_layer)),
                "list_with_ids",
            );
            black_box(rows.len())
        });
    });
    group.finish();
}

fn bench_mvt_tile(c: &mut Criterion, ctx: &Ctx, rt: &Runtime) {
    // Tile z12 sobre o canto SO da mancha de dados (-52°, -10°).
    let (x, y) = tile_xy(-52.0, -10.0, 12);
    let tile = MvtTile::new(12, x, y);
    let mut group = c.benchmark_group("mvt_tile");
    group.bench_function("z12_viewport", |b| {
        b.iter(|| {
            let bytes = must(
                rt.block_on(mvt_tile(&ctx.pool, &ctx.read_layer, &tile)),
                "mvt_tile",
            );
            black_box(bytes.map(|b| b.len()).unwrap_or(0))
        });
    });
    group.finish();
}

/// Converte lon/lat (graus, EPSG:4674) na coluna/linha do tile esquema XYZ.
fn tile_xy(lon: f64, lat: f64, z: i32) -> (i32, i32) {
    let n = 2f64.powi(z);
    let x = ((lon + 180.0) / 360.0 * n).floor();
    let lat_rad = lat.to_radians();
    let y = ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor();
    (x as i32, y as i32)
}
