//! Suíte de integração PostgreSQL/PostGIS — **gated** por `TA_TEST_PG=1`.
//!
//! Sem a variável definida, cada teste imprime "skipped" e passa, para não
//! quebrar `cargo test` em máquinas sem banco (economia de testes).
//!
//! Cluster de referência (harness): `scripts/test-pg.sh` — PG16 + PostGIS 3.4
//! efêmero via conda-forge, porta 55432. DSN configurável via `TA_PG_DSN`.
//!
//! Testes cobertos:
//! 1. migrações aplicam limpas num banco NOVO (criado e descartado no teste);
//! 2. round-trip de feature com SRID 4674;
//! 3. rejeição de SRID ≠ 4674 (erro explícito);
//! 4. bulk_insert de 10k features com tempo documentado no output;
//! 5. bbox_query retorna só o que intersecta.

use std::time::Instant;

use geo_types::{LineString, Point, Polygon};
use sqlx::PgPool;
use ta_core::feature::Feature;
use ta_core::geo;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::spatial::{TARGET_SRID, is_postgis_ready, run_migrations};
use tokio::sync::OnceCell;

/// DSN padrão = harness do `scripts/test-pg.sh`.
const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

static INIT: OnceCell<()> = OnceCell::const_new();

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

async fn init_schema() {
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    let pool = sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness (scripts/test-pg.sh)");
    if !is_postgis_ready(&pool)
        .await
        .expect("consultar pg_extension")
    {
        ta_db::spatial::ensure_postgis(&pool)
            .await
            .expect("PostGIS disponível no harness");
    }
    run_migrations(&pool).await.expect("migrações aplicam");
    pool.close().await;
}

/// Setup compartilhado: aplica migrações uma vez por processo de teste.
async fn setup() {
    INIT.get_or_init(init_schema).await;
}

async fn test_pool() -> PgPool {
    setup().await;
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness")
}

/// Nome único por teste (nanos + contador atômico) para rodar sem ordem fixa.
fn unique_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
}

async fn make_project_layer(pool: &PgPool) -> (Project, Layer) {
    let projects = PgProjectRepo::new(pool.clone());
    let layers = PgLayerRepo::new(pool.clone());

    let project = projects
        .create(&Project {
            id: String::new(),
            name: unique_name("proj"),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("cria projeto");

    let layer = layers
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("camada"),
            geom_type: GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "test://local".to_owned(),
        })
        .await
        .expect("cria camada");
    (project, layer)
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

#[tokio::test]
async fn migrações_aplicam_limpas_em_banco_novo() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let admin = test_pool().await;
    // Conecta ao banco `postgres` do mesmo servidor para criar um banco novo.
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    let admin_dsn = sibling_db_dsn(&dsn, "postgres");
    let admin2 = sqlx::PgPool::connect(&admin_dsn)
        .await
        .expect("conectar ao banco postgres administrativo");

    let db_name = unique_name("ta_mig_test").replace('_', "");
    let create_sql = format!("CREATE DATABASE {db_name}");
    sqlx::query(sqlx::AssertSqlSafe(create_sql))
        .execute(&admin2)
        .await
        .expect("cria banco de teste"); // AssertSqlSafe: nome gerado (prefixo+hex)

    let fresh_dsn = sibling_db_dsn(&dsn, &db_name);
    let fresh = sqlx::PgPool::connect(&fresh_dsn)
        .await
        .expect("conectar ao banco novo");
    if !is_postgis_ready(&fresh).await.unwrap() {
        ta_db::spatial::ensure_postgis(&fresh)
            .await
            .expect("postgis");
    }
    run_migrations(&fresh)
        .await
        .expect("migrações aplicam limpas");
    run_migrations(&fresh)
        .await
        .expect("migrações são idempotentes");

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables \
         WHERE table_schema = 'public' ORDER BY table_name",
    )
    .fetch_all(&fresh)
    .await
    .unwrap();
    for expected in ["features", "layers", "projects"] {
        assert!(
            tables.iter().any(|t| t == expected),
            "tabela {expected} ausente após migrações: {tables:?}"
        );
    }

    fresh.close().await;
    let drop_sql = format!("DROP DATABASE {db_name}");
    sqlx::query(sqlx::AssertSqlSafe(drop_sql))
        .execute(&admin2)
        .await
        .expect("descarta banco de teste");
    admin.close().await;
    admin2.close().await;
}

#[tokio::test]
async fn feature_roundtrip_srid_4674() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (_project, layer) = make_project_layer(&pool).await;
    let features = PgFeatureRepo::new(pool.clone());

    let geom: geo_types::Geometry<f64> = sample_polygon(-52.0, -11.0, 0.5).into();
    let inserted = features
        .bulk_insert(
            &layer.id,
            vec![Feature::new(
                geom.clone(),
                serde_json::json!({"nome": "poligono_teste", "area_ha": 12.5}),
            )],
        )
        .await
        .expect("bulk_insert de 1 feature");
    assert_eq!(inserted, 1);

    let got = features
        .bbox_query(&layer.id, -53.0, -12.0, -51.0, -10.0)
        .await
        .expect("bbox_query");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].geom, geom);
    assert_eq!(got[0].attrs["nome"], "poligono_teste");

    assert_eq!(features.count(&layer.id).await.unwrap(), 1);

    // update_bbox materializa o envelope da camada
    let layers = PgLayerRepo::new(pool.clone());
    assert!(layers.update_bbox(&layer.id).await.unwrap());
    let listed = layers.list_for_project(&_project.id).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, layer.id);

    pool.close().await;
}

#[tokio::test]
async fn rejeita_srid_diferente_de_4674() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (_project, layer) = make_project_layer(&pool).await;
    let features = PgFeatureRepo::new(pool.clone());

    // EWKB com SRID 4326 embutido — tem que ser rejeitado (typmod + CHECK).
    let geom: geo_types::Geometry<f64> = Point::new(-52.0, -11.0).into();
    let ewkb_4326 = geo::to_ewkb(&geom, 4326).expect("ewkb 4326");
    let hex = bytes_to_hex(&ewkb_4326);
    let err = sqlx::query(
        "INSERT INTO features (layer_id, geom, attrs) \
         SELECT $1::uuid, $2::text::geometry, '{}'::jsonb",
    )
    .bind(&layer.id)
    .bind(&hex)
    .execute(&pool)
    .await
    .expect_err("SRID 4326 deve ser rejeitado");
    let msg = err.to_string();
    assert!(
        msg.contains("4326"),
        "erro deve citar o SRID rejeitado: {msg}"
    );
    assert_eq!(features.count(&layer.id).await.unwrap(), 0);

    pool.close().await;
}

#[tokio::test]
async fn bulk_insert_10k_em_tempo_documentado() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    const N: usize = 10_000;
    /// Orçamento de tempo (folgado, para CI em máquina compartilhada).
    const BUDGET_SECS: u64 = 60;

    let pool = test_pool().await;
    let (_project, layer) = make_project_layer(&pool).await;
    let features = PgFeatureRepo::new(pool.clone());

    let data: Vec<Feature> = (0..N)
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
        .collect();

    let start = Instant::now();
    let inserted = features
        .bulk_insert(&layer.id, data)
        .await
        .expect("bulk_insert 10k");
    let elapsed = start.elapsed();
    println!(
        "bulk_insert de {N} features: {:.1} ms (orçamento documentado: {BUDGET_SECS}s; \
         caminho: COPY FROM STDIN com fallback batched)",
        elapsed.as_secs_f64() * 1000.0
    );
    assert_eq!(inserted as usize, N);
    assert!(
        elapsed.as_secs() < BUDGET_SECS,
        "bulk_insert excedeu o orçamento de {BUDGET_SECS}s: {elapsed:?}"
    );
    assert_eq!(features.count(&layer.id).await.unwrap() as usize, N);

    pool.close().await;
}

#[tokio::test]
async fn bbox_query_retorna_somente_intersectantes() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (_project, layer) = make_project_layer(&pool).await;
    let features = PgFeatureRepo::new(pool.clone());

    // Grade 5x5 de pontos: x ∈ {-54..-50}, y ∈ {-14..-10}
    let data: Vec<Feature> = (-2..=2)
        .flat_map(|dx| (-2..=2).map(move |dy| (dx, dy)))
        .map(|(dx, dy)| {
            Feature::new(
                geo_types::Geometry::Point(Point::new(-52.0 + dx as f64, -12.0 + dy as f64)),
                serde_json::json!({"dx": dx, "dy": dy}),
            )
        })
        .collect();
    let n = data.len() as u64;
    features.bulk_insert(&layer.id, data).await.unwrap();

    // Janela que cobre dx ∈ {-1,0} e dy ∈ {-1,0,1} → 2×3 = 6 pontos
    let got = features
        .bbox_query(&layer.id, -53.5, -13.5, -51.5, -10.5)
        .await
        .unwrap();
    assert_eq!(got.len(), 6, "janela 2x3 deve trazer 6 pontos");

    // Janela fora do dado
    let none = features
        .bbox_query(&layer.id, 0.0, 0.0, 1.0, 1.0)
        .await
        .unwrap();
    assert!(none.is_empty());

    assert_eq!(features.count(&layer.id).await.unwrap(), n);

    pool.close().await;
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Troca só o banco (último segmento do path), preservando scheme+authority.
fn sibling_db_dsn(dsn: &str, db: &str) -> String {
    match dsn.rsplit_once('/') {
        Some((prefix, _)) if prefix.ends_with("//") => format!("{dsn}/{db}"),
        Some((prefix, _)) => format!("{prefix}/{db}"),
        None => format!("{dsn}/{db}"),
    }
}

/// Fase 3 — edição pela GUI: list_with_ids dá o id de linha, e
/// update_attrs/delete_feature editam/removem por esse id (bind-only).
#[tokio::test]
async fn edicao_por_id_update_e_delete() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (_project, layer) = make_project_layer(&pool).await;
    let features = PgFeatureRepo::new(pool.clone());

    features
        .bulk_insert(
            &layer.id,
            vec![
                Feature::new(
                    geo_types::Geometry::Point(Point::new(-52.0, -12.0)),
                    serde_json::json!({"nome": "a", "v": 1}),
                ),
                Feature::new(
                    geo_types::Geometry::Point(Point::new(-51.0, -11.0)),
                    serde_json::json!({"nome": "b", "v": 2}),
                ),
            ],
        )
        .await
        .unwrap();

    let with_ids = features.list_with_ids(&layer.id).await.unwrap();
    assert_eq!(with_ids.len(), 2);
    let (id_b, feat_b) = &with_ids[1];
    assert_eq!(feat_b.attrs["nome"], "b");

    // update_attrs substitui o JSONB inteiro
    let ok = features
        .update_attrs(*id_b, &serde_json::json!({"nome": "b2", "v": 3}))
        .await
        .unwrap();
    assert!(ok, "update_attrs em id existente deve afetar 1 linha");
    let after = features.list_with_ids(&layer.id).await.unwrap();
    assert_eq!(after[1].1.attrs["nome"], "b2");
    assert_eq!(after[1].1.attrs["v"], 3);

    // id inexistente → Ok(false), sem erro
    assert!(
        !features
            .update_attrs(i64::MAX, &serde_json::json!({}))
            .await
            .unwrap()
    );
    assert!(!features.delete_feature(i64::MAX).await.unwrap());

    // delete_feature remove a linha certa
    let (id_a, _) = &after[0];
    assert!(features.delete_feature(*id_a).await.unwrap());
    let rest = features.list_with_ids(&layer.id).await.unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].0, *id_b);

    pool.close().await;
}
