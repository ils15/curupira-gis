//! Repositórios da **Fase 1** ainda sem cobertura — **gated** por `TA_TEST_PG=1`.
//!
//! Cluster de referência: `scripts/test-pg.sh` (PG16 + PostGIS 3.4, porta
//! 55432); DSN configurável por `TA_PG_DSN`. Sem a variável, cada teste
//! imprime "skipped" e passa (economia: não exige banco).
//!
//! Cobre o que não tinha teste algum: `PgPhaseRepo` (CRUD + nome duplicado),
//! `PgCellRepo` (list/exists/count + idempotência real do
//! `INSERT … ON CONFLICT DO NOTHING`), `PgBlockedCellRepo::unblock` e
//! `PgAreaSettingsRepo` (set/get mínimos).

use geo_types::{LineString, Polygon};
use sqlx::PgPool;
use ta_core::aoi::{AoiLayer, AreaSettings};
use ta_core::cell::{BlockedCell, Cell, CellLayer, GridSpec};
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::phase::Phase;
use ta_core::project::Project;
use ta_db::Error as DbError;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, AreaSettingsRepo, PgAoiLayerRepo, PgAreaSettingsRepo};
use ta_db::repo_cell::{
    BlockedCellRepo, CellLayerRepo, CellRepo, PgBlockedCellRepo, PgCellLayerRepo, PgCellRepo,
};
use ta_db::repo_phase::{PgPhaseRepo, PhaseRepo};
use ta_db::spatial::{TARGET_SRID, is_postgis_ready, run_migrations};
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

async fn test_pool() -> PgPool {
    INIT.get_or_init(init_schema).await;
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness")
}

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

/// Retângulo sintético como polígono fechado.
fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon<f64> {
    Polygon::new(
        LineString::from(vec![
            (min_x, min_y),
            (max_x, min_y),
            (max_x, max_y),
            (min_x, max_y),
            (min_x, min_y),
        ]),
        vec![],
    )
}

/// Projeto novo e isolado.
async fn new_project(pool: &PgPool, prefix: &str) -> Project {
    PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name(prefix),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("cria projeto")
}

/// Projeto + AOI + camada de células persistida com a grade do motor.
/// Devolve `(project, cell_layer)`.
async fn setup_grid(pool: &PgPool) -> (Project, CellLayer) {
    let project = new_project(pool, "fase1").await;
    let layer = PgLayerRepo::new(pool.clone())
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi"),
            geom_type: GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "test://fase1".into(),
        })
        .await
        .expect("cria camada de AOI");
    let area_poly = rect(-52.20, -3.20, -52.14, -3.16); // 0.06 × 0.04 graus
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(
                area_poly.clone().into(),
                serde_json::json!({}),
            )],
        )
        .await
        .expect("insere a geometria da AOI");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(&project.id, &layer.id, None))
        .await
        .expect("cria vínculo de AOI");

    let spec = GridSpec::new(0.01, -52.20, -3.20, TARGET_SRID as i32);
    let area = geo_types::MultiPolygon::new(vec![area_poly]);
    let generated = ta_engine::generate_grid(&area, &spec).expect("grid do motor");
    let cells: Vec<Cell> = generated
        .iter()
        .map(|c| Cell {
            id: 0,
            cell_layer_id: String::new(),
            cell_id: c.cell_id.clone(),
            geom: c.geom.clone(),
            row_index: c.row_index,
            col_index: c.col_index,
        })
        .collect();

    let cell_layer = CellLayer::new(&project.id, &aoi.id, &unique_name("grade"), &spec);
    let saved = PgCellLayerRepo::new(pool.clone())
        .create(&cell_layer)
        .await
        .expect("cria camada de células");
    PgCellRepo::new(pool.clone())
        .bulk_insert(&saved.id, &cells, spec.srid)
        .await
        .expect("insere células");
    (project, saved)
}

#[tokio::test]
async fn phase_repo_create_get_list_delete() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase").await;
    let repo = PgPhaseRepo::new(pool.clone());

    let mut phase = Phase::new(&project.id, "revisao");
    phase.sort_order = 7;
    phase.cell_blocker = true;
    let saved = repo.create(&phase).await.expect("cria fase");
    assert_eq!(saved.project_id, project.id);
    assert!(saved.cell_blocker);

    let fetched = repo.get(&saved.id).await.expect("get").expect("existe");
    assert_eq!(fetched.id, saved.id);
    assert_eq!(fetched.name, "revisao");
    assert_eq!(fetched.sort_order, 7);

    let listed = repo.list_for_project(&project.id).await.expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, saved.id);

    assert!(repo.delete(&saved.id).await.expect("delete"));
    assert!(repo.get(&saved.id).await.expect("get pós-delete").is_none());
    assert!(!repo.delete(&saved.id).await.expect("delete ausente"));
    pool.close().await;
}

#[tokio::test]
async fn phase_nome_duplicado_no_mesmo_projeto_e_violacao_de_unicidade() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase").await;
    let repo = PgPhaseRepo::new(pool.clone());
    repo.create(&Phase::new(&project.id, "unica"))
        .await
        .expect("1ª fase");

    let err = repo
        .create(&Phase::new(&project.id, "unica"))
        .await
        .expect_err("nome duplicado deve falhar");
    match err {
        DbError::Sql(sqlx::Error::Database(db)) => {
            assert!(db.is_unique_violation(), "esperava violação de unicidade");
        }
        other => panic!("esperava erro de banco, veio {other:?}"),
    }

    // O MESMO nome em outro projeto é permitido (UNIQUE é por projeto).
    let other = new_project(&pool, "fase").await;
    repo.create(&Phase::new(&other.id, "unica"))
        .await
        .expect("mesmo nome em outro projeto é válido");
    pool.close().await;
}

#[tokio::test]
async fn cell_repo_list_exists_count_e_insert_idempotente() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, cell_layer) = setup_grid(&pool).await;
    let repo = PgCellRepo::new(pool.clone());

    let count = repo.count(&cell_layer.id).await.expect("count");
    assert!(count > 0, "a grade deveria ter células");
    assert_eq!(
        repo.list(&cell_layer.id).await.expect("list").len() as u64,
        count
    );

    // exists por cell_id (positivo e negativo).
    let first_id = repo.list(&cell_layer.id).await.expect("list")[0]
        .cell_id
        .clone();
    assert!(
        repo.exists(&cell_layer.id, &first_id)
            .await
            .expect("exists")
    );
    assert!(
        !repo
            .exists(&cell_layer.id, "999_999")
            .await
            .expect("exists ausente")
    );

    // Idempotência real do `INSERT … ON CONFLICT DO NOTHING`: reinserir a grade
    // inteira insere 0 novas linhas e não altera a contagem.
    let spec = GridSpec::new(0.01, -52.20, -3.20, TARGET_SRID as i32);
    let area = geo_types::MultiPolygon::new(vec![rect(-52.20, -3.20, -52.14, -3.16)]);
    let again: Vec<Cell> = ta_engine::generate_grid(&area, &spec)
        .expect("grid")
        .iter()
        .map(|c| Cell {
            id: 0,
            cell_layer_id: String::new(),
            cell_id: c.cell_id.clone(),
            geom: c.geom.clone(),
            row_index: c.row_index,
            col_index: c.col_index,
        })
        .collect();
    let inserted = repo
        .bulk_insert(&cell_layer.id, &again, spec.srid)
        .await
        .expect("reinsert");
    assert_eq!(
        inserted, 0,
        "ON CONFLICT DO NOTHING não deve inserir de novo"
    );
    assert_eq!(
        repo.count(&cell_layer.id).await.expect("count"),
        count,
        "contagem estável após reinsert"
    );

    PgProjectRepo::new(pool.clone())
        .delete(&project.id)
        .await
        .expect("cleanup");
    pool.close().await;
}

#[tokio::test]
async fn blocked_cell_unblock_remove_o_bloqueio() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, cell_layer) = setup_grid(&pool).await;
    let cells = PgCellRepo::new(pool.clone());
    let blocked = PgBlockedCellRepo::new(pool.clone());

    let target = cells.list(&cell_layer.id).await.expect("list")[0]
        .cell_id
        .clone();
    blocked
        .block(&BlockedCell::new(
            &cell_layer.id,
            &target,
            Some("nuvem".into()),
        ))
        .await
        .expect("bloqueia");
    assert_eq!(blocked.list(&cell_layer.id).await.expect("list").len(), 1);

    assert!(
        blocked
            .unblock(&cell_layer.id, &target)
            .await
            .expect("unblock"),
        "a célula estava bloqueada"
    );
    assert!(blocked.list(&cell_layer.id).await.expect("list").is_empty());
    assert!(
        !blocked
            .unblock(&cell_layer.id, &target)
            .await
            .expect("unblock de novo"),
        "nada a remover na segunda vez"
    );

    PgProjectRepo::new(pool.clone())
        .delete(&project.id)
        .await
        .expect("cleanup");
    pool.close().await;
}

#[tokio::test]
async fn area_settings_set_get_e_upsert() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "area").await;
    let repo = PgAreaSettingsRepo::new(pool.clone());

    assert!(
        repo.get(&project.id).await.expect("get vazio").is_none(),
        "projeto novo não tem configuração de área"
    );

    let saved = repo
        .set(&AreaSettings::new(&project.id))
        .await
        .expect("set");
    assert_eq!(saved.srs_id, 5880);
    assert_eq!(saved.unit, "ha");

    // Upsert por UNIQUE(project_id): atualiza a unidade sem duplicar.
    let mut updated = AreaSettings::new(&project.id);
    updated.unit = "km2".to_owned();
    repo.set(&updated).await.expect("upsert");
    let got = repo.get(&project.id).await.expect("get").expect("existe");
    assert_eq!(got.unit, "km2");
    assert_eq!(got.id, saved.id, "upsert reusa a mesma linha");

    PgProjectRepo::new(pool.clone())
        .delete(&project.id)
        .await
        .expect("cleanup");
    pool.close().await;
}

/// Projeto + AOI + camada de células **vazia** (sem células), para exercitar o
/// `bulk_insert` isoladamente. Devolve `(project, cell_layer)`.
async fn setup_empty_grid(pool: &PgPool) -> (Project, CellLayer) {
    let project = new_project(pool, "vazio").await;
    let layer = PgLayerRepo::new(pool.clone())
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi"),
            geom_type: GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "test://empty-grid".into(),
        })
        .await
        .expect("cria camada de AOI");
    let area_poly = rect(-52.20, -3.20, -52.14, -3.16);
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(area_poly.into(), serde_json::json!({}))],
        )
        .await
        .expect("insere a geometria da AOI");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(&project.id, &layer.id, None))
        .await
        .expect("cria vínculo de AOI");
    let spec = GridSpec::new(0.01, -52.20, -3.20, TARGET_SRID as i32);
    let cell_layer = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project.id,
            &aoi.id,
            &unique_name("vazia"),
            &spec,
        ))
        .await
        .expect("cria camada de células vazia");
    (project, cell_layer)
}

fn cell(cell_id: &str, geom: Polygon<f64>) -> Cell {
    Cell {
        id: 0,
        cell_layer_id: String::new(),
        cell_id: cell_id.to_owned(),
        geom,
        row_index: 0,
        col_index: 0,
    }
}

/// m2: o gate `cells_geom_validity` (0008) rejeita célula inválida — mesma
/// política de `features` — com mensagem acionável, em vez de persistir um
/// polígono que quebraria área/intersecção.
#[tokio::test]
async fn celula_invalida_e_rejeitada_com_mensagem_acionavel() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, cell_layer) = setup_empty_grid(&pool).await;

    // Polígono "bowtie" (auto-interseção): ST_IsValid = false.
    let bowtie = Polygon::new(
        LineString::from(vec![
            (0.0, 0.0),
            (1.0, 1.0),
            (1.0, 0.0),
            (0.0, 1.0),
            (0.0, 0.0),
        ]),
        vec![],
    );
    let err = PgCellRepo::new(pool.clone())
        .bulk_insert(
            &cell_layer.id,
            &[cell("bowtie", bowtie)],
            TARGET_SRID as i32,
        )
        .await
        .expect_err("célula inválida deve ser rejeitada");
    let msg = err.to_string();
    assert!(
        msg.contains("geometria de célula inválida"),
        "mensagem sem a causa acionável: {msg}"
    );
    assert!(
        msg.contains("bowtie"),
        "mensagem deve identificar a célula: {msg}"
    );

    assert_eq!(
        PgCellRepo::new(pool.clone())
            .count(&cell_layer.id)
            .await
            .expect("count"),
        0,
        "nada pode ser gravado quando a célula é rejeitada"
    );

    PgProjectRepo::new(pool.clone())
        .delete(&project.id)
        .await
        .expect("cleanup");
    pool.close().await;
}

/// m3: `bulk_insert` é **atômico** — 1001 células (2 lotes de `CHUNK`), a
/// última inválida. Sem transação, o lote 1 (1000 células) ficaria gravado e o
/// seed (idempotência por `count > 0`) nunca repararia a grade.
#[tokio::test]
async fn bulk_insert_e_atomico_quando_um_lote_falha() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, cell_layer) = setup_empty_grid(&pool).await;

    let mut cells: Vec<Cell> = (0..1001)
        .map(|i| {
            let x = f64::from(i) * 0.001;
            cell(&format!("c{i}"), rect(x, 0.0, x + 0.0005, 0.0005))
        })
        .collect();
    // Índice 1000 = PRIMEIRA célula do segundo lote: a falha acontece depois de
    // o lote 1 já ter sido executado na conexão.
    cells[1000] = cell(
        "bowtie",
        Polygon::new(
            LineString::from(vec![
                (0.0, 0.0),
                (1.0, 1.0),
                (1.0, 0.0),
                (0.0, 1.0),
                (0.0, 0.0),
            ]),
            vec![],
        ),
    );

    let err = PgCellRepo::new(pool.clone())
        .bulk_insert(&cell_layer.id, &cells, TARGET_SRID as i32)
        .await
        .expect_err("o lote com célula inválida deve falhar");
    assert!(
        err.to_string().contains("geometria de célula inválida"),
        "falha inesperada: {err}"
    );

    assert_eq!(
        PgCellRepo::new(pool.clone())
            .count(&cell_layer.id)
            .await
            .expect("count"),
        0,
        "rollback total: o lote 1 não pode ficar gravado parcialmente"
    );

    PgProjectRepo::new(pool.clone())
        .delete(&project.id)
        .await
        .expect("cleanup");
    pool.close().await;
}

/// m5: o SRS de entrada é explícito e validado contra `cell_layers.srid` —
/// passar a célula em outro SRS é erro, não reprojeção silenciosa de lixo.
#[tokio::test]
async fn bulk_insert_rejeita_srid_divergente_da_camada() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let (project, cell_layer) = setup_empty_grid(&pool).await;

    let err = PgCellRepo::new(pool.clone())
        .bulk_insert(
            &cell_layer.id,
            &[cell("c0", rect(0.0, 0.0, 0.001, 0.001))],
            5880,
        )
        .await
        .expect_err("SRS divergente deve ser rejeitado");
    assert!(
        matches!(
            &err,
            DbError::SridLayerMismatch {
                input: 5880,
                layer: 4674
            }
        ),
        "erro tipado esperado, veio: {err}"
    );
    assert!(
        err.to_string().contains("SRS de entrada EPSG:5880"),
        "mensagem acionável ausente: {err}"
    );

    assert_eq!(
        PgCellRepo::new(pool.clone())
            .count(&cell_layer.id)
            .await
            .expect("count"),
        0
    );

    PgProjectRepo::new(pool.clone())
        .delete(&project.id)
        .await
        .expect("cleanup");
    pool.close().await;
}
