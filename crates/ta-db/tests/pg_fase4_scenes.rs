//! Repositórios de **cenas** e proveniência de medidas (Fase 4) — **gated**
//! por `TA_TEST_PG=1` no harness de `scripts/test-pg.sh`.
//!
//! Cobertura: migração `0014_scenes` aplica (a suíte toda depende dela),
//! `UNIQUE(name)` de cena é erro tipado, footprint faz round-trip EWKB↔PostGIS
//! como MultiPolygon 4674, vínculo `project_scenes` (idempotente, CASCADE),
//! `cell_measures.scene_id` como proveniência (SET NULL ao apagar a cena — a
//! medida permanece) e `upsert_measures_on` last-write-wins da proveniência.

use geo_types::{Coord, LineString, MultiPolygon, Polygon};
use sqlx::PgPool;
use ta_core::classification::CellMeasure;
use ta_core::project::Project;
use ta_db::repo::{PgProjectRepo, ProjectRepo};
use ta_db::repo_scene::{PgSceneRepo, SceneRepo};
use ta_db::repo_task::PgTaskRepo;
use ta_db::spatial::{is_postgis_ready, run_migrations};
use tokio::sync::OnceCell;

const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

static INIT: OnceCell<()> = OnceCell::const_new();

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

async fn init_schema() {
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    let pool = sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness");
    if !is_postgis_ready(&pool).await.expect("pg_extension") {
        ta_db::spatial::ensure_postgis(&pool)
            .await
            .expect("PostGIS disponível no harness");
    }
    run_migrations(&pool)
        .await
        .expect("migrações aplicam (14/14)");
    pool.close().await;
}

async fn test_pool() -> PgPool {
    INIT.get_or_init(init_schema).await;
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    sqlx::PgPool::connect(&dsn).await.expect("conectar no PG")
}

fn unique_name(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("relógio antes de 1970")
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
}

fn footprint(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> MultiPolygon<f64> {
    MultiPolygon::new(vec![Polygon::new(
        LineString::from(vec![
            Coord { x: min_x, y: min_y },
            Coord { x: max_x, y: min_y },
            Coord { x: max_x, y: max_y },
            Coord { x: min_x, y: max_y },
            Coord { x: min_x, y: min_y },
        ]),
        vec![],
    )])
}

#[tokio::test]
async fn cena_create_get_e_nome_duplicado_tipado() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = test_pool().await;
    let repo = PgSceneRepo::new(pool.clone());

    let name = unique_name("cena_unica");
    let mut scene = ta_core::scene::Scene::new(&name);
    scene.metadata = serde_json::json!({"satellite": "S2"});
    let saved = repo.create(&scene).await.expect("cena criada");
    assert!(!saved.id.is_empty());
    let got = repo.get_by_name(&name).await.expect("get_by_name");
    assert_eq!(got.as_ref().map(|s| s.id.as_str()), Some(saved.id.as_str()));

    // Nome duplicado: erro tipado com o caminho de saída na mensagem.
    let err = repo
        .create(&ta_core::scene::Scene::new(&name))
        .await
        .expect_err("duplicado recusado");
    let msg = err.to_string();
    assert!(msg.contains(&name) && msg.contains("scene link"), "{msg}");

    // Nome vazio: erro acionável antes do banco.
    let err = repo
        .create(&ta_core::scene::Scene::new("   "))
        .await
        .expect_err("nome vazio recusado");
    assert!(err.to_string().contains("--name"), "{err}");

    repo.delete(&saved.id).await.expect("cleanup delete");
    pool.close().await;
}

#[tokio::test]
async fn footprint_faz_round_trip_como_multipolygon_4674() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = test_pool().await;
    let repo = PgSceneRepo::new(pool.clone());

    let name = unique_name("cena_fp");
    let mut scene = ta_core::scene::Scene::new(&name);
    scene.footprint = Some(footprint(-52.0, -12.0, -51.9, -11.9));
    let saved = repo.create(&scene).await.expect("cena com footprint");

    let got = repo
        .get(&saved.id)
        .await
        .expect("get")
        .expect("cena existe");
    let fp = got.footprint.expect("footprint voltou");
    assert_eq!(fp.0.len(), 1);
    let ring = &fp.0[0].exterior().0;
    assert!(
        ring.iter().any(|c| (c.x - (-52.0)).abs() < 1e-12),
        "coordenadas preservadas: {ring:?}"
    );

    // O banco guarda MultiPolygon SRID 4674 — prova pelo typmod.
    let (srid, gtype): (i32, String) = sqlx::query_as(
        "SELECT ST_SRID(footprint)::int4, GeometryType(footprint) \
         FROM scenes WHERE id = $1::uuid",
    )
    .bind(&saved.id)
    .fetch_one(&pool)
    .await
    .expect("consulta do footprint");
    assert_eq!((srid, gtype.as_str()), (4674, "MULTIPOLYGON"));

    repo.delete(&saved.id).await.expect("cleanup delete");
    pool.close().await;
}

#[tokio::test]
async fn vinculo_de_cena_e_idempotente_e_remove_em_cascade() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = test_pool().await;
    let repo = PgSceneRepo::new(pool.clone());

    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name("proj_cena"),
            srid: 4674,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("projeto");
    let scene = repo
        .create(&ta_core::scene::Scene::new(&unique_name("cena_v")))
        .await
        .expect("cena");

    assert!(repo.link(&project.id, &scene.id).await.expect("link novo"));
    assert!(
        !repo
            .link(&project.id, &scene.id)
            .await
            .expect("link repetido é no-op")
    );
    let listadas = repo
        .list(Some(&project.id))
        .await
        .expect("list por projeto");
    assert_eq!(listadas.len(), 1);
    let usando = repo
        .projects_using(&scene.id)
        .await
        .expect("projects_using");
    assert_eq!(usando, vec![project.name.clone()]);

    // Apagar o projeto desvincula em CASCADE (a cena permanece).
    sqlx::query("DELETE FROM projects WHERE id = $1::uuid")
        .bind(&project.id)
        .execute(&pool)
        .await
        .expect("delete projeto");
    let sobrou = repo.get(&scene.id).await.expect("get cena");
    assert!(sobrou.is_some(), "cena sobrevive ao projeto");

    repo.delete(&scene.id).await.expect("cleanup cena");
    pool.close().await;
}

#[tokio::test]
async fn medida_com_cena_sobrevive_ao_delete_da_cena_set_null() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = test_pool().await;
    let repo = PgSceneRepo::new(pool.clone());

    // Projeto + camada de células + 1 célula (fixture mínimo de medida, via repos).
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name("proj_med"),
            srid: 4674,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("projeto");
    use ta_core::cell::{Cell, CellLayer, GridSpec};
    use ta_core::feature::Feature;
    use ta_core::layer::{GeomType, Layer};
    use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo};
    use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
    use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
    let layer = PgLayerRepo::new(pool.clone())
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi_med"),
            geom_type: GeomType::Polygon,
            srid: 4674,
            source_uri: "teste-fase4-db".to_owned(),
        })
        .await
        .expect("camada AOI");
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(
                geo_types::Polygon::new(
                    geo_types::LineString::from(vec![
                        (-52.0, -12.0),
                        (-51.99, -12.0),
                        (-51.99, -11.99),
                        (-52.0, -11.99),
                        (-52.0, -12.0),
                    ]),
                    vec![],
                )
                .into(),
                serde_json::json!({"cod": 1}),
            )],
        )
        .await
        .expect("feature");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&ta_core::aoi::AoiLayer::new(&project.id, &layer.id, None))
        .await
        .expect("vínculo de AOI");
    let cell_layer = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project.id,
            &aoi.id,
            &unique_name("grade_med"),
            &GridSpec::new(0.005, -52.0, -12.0, 4674),
        ))
        .await
        .expect("camada de células");
    let cell_layer_id = cell_layer.id.clone();
    PgCellRepo::new(pool.clone())
        .bulk_insert(
            &cell_layer_id,
            &[Cell {
                id: 0,
                cell_layer_id: cell_layer_id.clone(),
                cell_id: "0_0".to_owned(),
                geom: geo_types::Polygon::new(
                    geo_types::LineString::from(vec![
                        (-52.0, -12.0),
                        (-51.995, -12.0),
                        (-51.995, -11.995),
                        (-52.0, -11.995),
                        (-52.0, -12.0),
                    ]),
                    vec![],
                ),
                row_index: 0,
                col_index: 0,
            }],
            4674,
        )
        .await
        .expect("célula");

    let scene = repo
        .create(&ta_core::scene::Scene::new(&unique_name("cena_med")))
        .await
        .expect("cena");
    let now = time::OffsetDateTime::now_utc();
    let medida = |scene_id: Option<String>| CellMeasure {
        cell_layer_id: cell_layer_id.clone(),
        cell_id: "0_0".to_owned(),
        measure: "ndvi".to_owned(),
        value: 0.72,
        source: Some("teste".to_owned()),
        scene_id,
        updated_on: now,
    };

    // 1) Grava com cena → list_measures devolve o id da cena.
    let mut tx = pool.begin().await.expect("tx");
    PgTaskRepo::upsert_measures_on(&mut *tx, &[medida(Some(scene.id.clone()))])
        .await
        .expect("upsert com cena");
    tx.commit().await.expect("commit");
    let listadas = PgTaskRepo::new(pool.clone())
        .list_measures(&cell_layer_id, None)
        .await
        .expect("list measures");
    assert_eq!(listadas[0].scene_id.as_deref(), Some(scene.id.as_str()));

    // 2) Regrava SEM cena: last-write-wins — a proveniência é SUBSTITUÍDA.
    let mut tx = pool.begin().await.expect("tx");
    PgTaskRepo::upsert_measures_on(&mut *tx, &[medida(None)])
        .await
        .expect("upsert sem cena");
    tx.commit().await.expect("commit");
    let listadas = PgTaskRepo::new(pool.clone())
        .list_measures(&cell_layer_id, None)
        .await
        .expect("list measures");
    assert!(listadas[0].scene_id.is_none(), "última escrita vence");

    // 3) Regrava com cena e APAGA a cena: medida permanece, proveniência NULL.
    let mut tx = pool.begin().await.expect("tx");
    PgTaskRepo::upsert_measures_on(&mut *tx, &[medida(Some(scene.id.clone()))])
        .await
        .expect("upsert com cena");
    tx.commit().await.expect("commit");
    let medidas_antes = repo
        .measures_using(&scene.id)
        .await
        .expect("measures_using");
    assert_eq!(medidas_antes, 1);
    repo.delete(&scene.id).await.expect("delete cena");
    let listadas = PgTaskRepo::new(pool.clone())
        .list_measures(&cell_layer_id, None)
        .await
        .expect("list measures");
    assert_eq!(listadas.len(), 1, "medida permanece");
    assert!(listadas[0].scene_id.is_none(), "proveniência desvinculada");

    // Cleanup: camada (cascateia células/medidas/AOI) e projeto.
    sqlx::query("DELETE FROM cell_layers WHERE id = $1::uuid")
        .bind(&cell_layer_id)
        .execute(&pool)
        .await
        .expect("cleanup grade");
    sqlx::query("DELETE FROM projects WHERE id = $1::uuid")
        .bind(&project.id)
        .execute(&pool)
        .await
        .expect("cleanup projeto");
    pool.close().await;
}
