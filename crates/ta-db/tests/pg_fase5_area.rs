//! Repositórios do **cálculo de área consolidado** (Fase 5, Decisão 4) —
//! **gated** por `TA_TEST_PG=1` (harness de `scripts/test-pg.sh`).
//!
//! Fixture: a AOI real do demo (`testdata/monitoring/aoi.geojson`,
//! Altamira/PA, lat ≈ −3.19) com a grade de **0.005°** — as 118 células do
//! fixture do `seed-demo`. Cobertura: resumo coerente
//! (`available + blocked == total`), área por classe do rascunho, teste de
//! **ordem de grandeza 5880 vs 4674** (razão ≈ (111320·cos φ)·111320 dentro
//! de ~1%), SRS inválido/geográfico recusado e consistência por célula.

use std::collections::BTreeSet;

use geo_types::{Coord, LineString, MultiPolygon, Polygon};
use sqlx::PgPool;
use ta_core::aoi::AoiLayer;
use ta_core::cell::{BlockedCell, Cell, CellLayer, GridSpec};
use ta_core::classification::{Class, DraftLayer};
use ta_core::feature::Feature;
use ta_core::project::Project;
use ta_core::rule::RuleSet;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_area::{AreaRepo, PgAreaRepo};
use ta_db::repo_cell::{
    BlockedCellRepo, CellLayerRepo, CellRepo, PgBlockedCellRepo, PgCellLayerRepo, PgCellRepo,
};
use ta_db::repo_class::{ClassRepo, DraftLayerRepo, PgClassRepo, PgDraftLayerRepo};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};
use ta_db::repo_task::PgTaskRepo;
use ta_db::spatial::{TARGET_SRID, is_postgis_ready, run_migrations};
use ta_engine::{aoi_bbox, generate_grid};
use tokio::sync::OnceCell;

const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

/// Resolução da grade do fixture do demo (`seed-demo`): 0.005° → 118 células.
const RES: f64 = 0.005;

/// A AOI do fixture (`testdata/monitoring/aoi.geojson`), Altamira/PA — os 6
/// vértices do anel (o fechamento é do `Polygon`).
const AOI_FIXTURE: [(f64, f64); 6] = [
    (-52.205, -3.215),
    (-52.160, -3.220),
    (-52.142, -3.192),
    (-52.150, -3.165),
    (-52.178, -3.158),
    (-52.202, -3.178),
];

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
        .expect("relógio antes de 1970")
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
}

/// O polígono da AOI do fixture, em 4674.
fn aoi_fixture() -> MultiPolygon<f64> {
    MultiPolygon::new(vec![Polygon::new(
        LineString::from(
            AOI_FIXTURE
                .iter()
                .map(|(x, y)| Coord { x: *x, y: *y })
                .collect::<Vec<_>>(),
        ),
        vec![],
    )])
}

/// Cleanup na ordem das RESTRICT (`draft_cells.ruleset/class`): draft_cells
/// primeiro, depois rulesets e o projeto (cascateia o resto).
async fn cleanup(pool: &PgPool, project_id: &str) {
    sqlx::query(
        "DELETE FROM draft_cells WHERE draft_layer_id IN \
         (SELECT id FROM draft_layers WHERE project_id = $1::uuid)",
    )
    .bind(project_id)
    .execute(pool)
    .await
    .expect("cleanup de draft_cells");
    sqlx::query("DELETE FROM rulesets WHERE project_id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup de rulesets");
    PgProjectRepo::new(pool.clone())
        .delete(project_id)
        .await
        .expect("cleanup do projeto");
}

/// Projeto + AOI (fixture) + grade 0.005° (as 118 células do demo) em 4674.
/// Devolve projeto, id da grade e os ids de célula ordenados.
async fn make_fixture_grid(pool: &PgPool, prefix: &str) -> (Project, String, Vec<String>) {
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name(prefix),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("cria projeto");
    let layer = PgLayerRepo::new(pool.clone())
        .add(&ta_core::layer::Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi"),
            geom_type: ta_core::layer::GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "test://fase5".into(),
        })
        .await
        .expect("cria camada de AOI");
    let area = aoi_fixture();
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(
                area.clone().into(),
                serde_json::json!({"nome": "aoi"}),
            )],
        )
        .await
        .expect("insere a geometria da AOI");
    let aoi = PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(&project.id, &layer.id, None))
        .await
        .expect("cria vínculo de AOI");

    let [origin_x, origin_y, ..] = aoi_bbox(&area).expect("bbox da AOI");
    let spec = GridSpec::new(RES, origin_x, origin_y, TARGET_SRID as i32);
    let generated = generate_grid(&area, &spec).expect("grade pura");
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
    let saved = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project.id,
            &aoi.id,
            &unique_name("grade"),
            &spec,
        ))
        .await
        .expect("cria camada de células");
    let inserted = PgCellRepo::new(pool.clone())
        .bulk_insert(&saved.id, &cells, TARGET_SRID as i32)
        .await
        .expect("insere células");
    assert_eq!(inserted, cells.len() as u64);
    let mut ids: Vec<String> = cells.iter().map(|c| c.cell_id.clone()).collect();
    ids.sort();
    (project, saved.id, ids)
}

/// Bloqueia as duas primeiras células (determinístico).
async fn block_two(pool: &PgPool, cell_layer_id: &str, ids: &[String]) {
    for cell_id in &ids[..2] {
        PgBlockedCellRepo::block(
            &PgBlockedCellRepo::new(pool.clone()),
            &BlockedCell::new(cell_layer_id, cell_id, Some("teste".into())),
        )
        .await
        .expect("bloqueia célula");
    }
}

/// Ruleset + 2 classes + rascunho com `n_a` células na classe A e `n_b` na B.
/// Devolve (nome do rascunho, classes usadas).
async fn classify_cells(
    pool: &PgPool,
    project_id: &str,
    cell_layer_id: &str,
    ids: &[String],
    n_a: usize,
    n_b: usize,
) -> (String, [String; 2]) {
    let class_a = PgClassRepo::new(pool.clone())
        .create(&Class::new(project_id, &unique_name("classe_a")))
        .await
        .expect("cria classe A");
    let class_b = PgClassRepo::new(pool.clone())
        .create(&Class::new(project_id, &unique_name("classe_b")))
        .await
        .expect("cria classe B");
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(project_id, &unique_name("rs")))
        .await
        .expect("cria ruleset");
    // Regras de proveniência (o upsert de draft_cells referencia o ruleset).
    for (class_id, priority) in [(class_a.id.clone(), 1), (class_b.id.clone(), 2)] {
        PgRuleRepo::new(pool.clone())
            .create(&ta_core::rule::Rule {
                id: String::new(),
                ruleset_id: ruleset.id.clone(),
                class_id,
                name: unique_name("regra"),
                priority,
                predicate: "ndvi >= 0.6".to_owned(),
                is_active: true,
                created_at: time::OffsetDateTime::now_utc(),
                updated_at: time::OffsetDateTime::now_utc(),
            })
            .await
            .expect("cria regra");
    }
    let draft = PgDraftLayerRepo::new(pool.clone())
        .create(
            &DraftLayer::new(project_id, &unique_name("rasc")).with_cell_layer(Some(cell_layer_id)),
        )
        .await
        .expect("cria rascunho");
    let rows: Vec<(String, String, String)> = ids[..n_a]
        .iter()
        .map(|c| (cell_layer_id.to_owned(), c.clone(), class_a.id.clone()))
        .chain(
            ids[n_a..n_a + n_b]
                .iter()
                .map(|c| (cell_layer_id.to_owned(), c.clone(), class_b.id.clone())),
        )
        .collect();
    let mut tx = pool.begin().await.expect("tx");
    PgTaskRepo::draft_upsert_on(&mut *tx, &draft.id, &ruleset.id, &rows)
        .await
        .expect("grava decisões");
    tx.commit().await.expect("commit das decisões");
    (draft.name, [class_a.name, class_b.name])
}

// ---------------------------------------------------------------------------

/// Resumo coerente: uma query, contagens certas e
/// `available_m2 + blocked_m2 == total_m2` (invariante do produtor; a
/// subtração f64 admite 1 ulp — tolerância relativa 1e-9).
#[tokio::test]
async fn area_summary_e_coerente() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, ids) = make_fixture_grid(&pool, "f5-area").await;
    block_two(&pool, &grid_id, &ids).await;

    let summary = PgAreaRepo::new(pool.clone())
        .area_summary_on(&grid_id, 5880)
        .await
        .expect("área no SRS métrico do projeto");

    assert_eq!(
        summary.cells_total, 118,
        "a grade do fixture tem 118 células"
    );
    assert_eq!(summary.cells_blocked, 2);
    let soma = summary.available_m2 + summary.blocked_m2;
    assert!(
        (soma - summary.total_m2).abs() <= summary.total_m2.abs() * 1e-9,
        "available + blocked deve ser total (diferença de {:e})",
        soma - summary.total_m2
    );
    assert!(summary.total_m2 > 0.0);
    assert!(summary.blocked_m2 > 0.0);
    assert!(summary.available_m2 < summary.total_m2);

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// Área por classe do rascunho: contagens por classe batem com as decisões e
/// a soma por classe cabe na área disponível (decidir não cria área).
#[tokio::test]
async fn area_por_classe_do_rascunho() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, ids) = make_fixture_grid(&pool, "f5-class").await;
    let (draft_name, _) = classify_cells(&pool, &project.id, &grid_id, &ids, 3, 2).await;

    let drafts = PgDraftLayerRepo::new(pool.clone())
        .list_for_project(&project.id)
        .await
        .expect("lista rascunhos");
    let draft = drafts
        .iter()
        .find(|d| d.name == draft_name)
        .expect("rascunho existe");
    let by_class = PgAreaRepo::new(pool.clone())
        .area_by_class_on(&draft.id, 5880)
        .await
        .expect("área por classe");

    assert_eq!(by_class.len(), 2, "duas classes decididas");
    assert!(
        by_class
            .windows(2)
            .all(|w| w[0].class_name <= w[1].class_name),
        "ordenado por nome de classe"
    );
    let mut total_cells = 0;
    let mut soma_m2 = 0.0;
    for class in &by_class {
        assert!(class.area_m2 > 0.0, "classe decidida tem área positiva");
        total_cells += class.cells;
        soma_m2 += class.area_m2;
    }
    assert_eq!(total_cells, 5, "3 + 2 células decididas");

    let summary = PgAreaRepo::new(pool.clone())
        .area_summary_on(&grid_id, 5880)
        .await
        .expect("resumo");
    assert!(
        soma_m2 <= summary.available_m2 + summary.available_m2.abs() * 1e-9,
        "soma por classe ≤ disponível (sem sobreposição de decisões)"
    );

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// **Critério de aceite do plano (ordem de grandeza 5880 vs 4674):** para a
/// AOI do fixture (lat ≈ −3.19), `area_m2 / area_graus²` bate com
/// `(111_320·cos φ)·111_320` dentro de ~1%. Prova que o cálculo está no SRS
/// métrico (não em 4674) e que o número é plausível (~0.06°×0.06° ≈ 44 km²).
#[tokio::test]
async fn ordem_de_grandeza_5880_vs_4674() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, _) = make_fixture_grid(&pool, "f5-ordem").await;

    let summary = PgAreaRepo::new(pool.clone())
        .area_summary_on(&grid_id, 5880)
        .await
        .expect("área métrica");
    // Área em graus² calculada AQUI no teste (a API recusa graus — Decisão 4);
    // `cells.geom` é 4674, então `ST_Area` planar devolve °².
    let area_graus2: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(ST_Area(geom)), 0::float8) \
         FROM cells WHERE cell_layer_id = $1::uuid",
    )
    .bind(&grid_id)
    .fetch_one(&pool)
    .await
    .expect("área em graus²");

    let ratio = summary.total_m2 / area_graus2;
    let lat = -3.19_f64;
    let esperado = (111_320.0 * lat.to_radians().cos()) * 111_320.0;
    let desvio = (ratio / esperado - 1.0).abs();
    assert!(
        desvio < 0.01,
        "razão m²/°² = {ratio:.0} deve ficar a ~1% do fator métrico \
         {esperado:.0} (desvio {:.2}%)",
        desvio * 100.0
    );
    // Ordem de grandeza do aceite: AOI ~0.06°×0.06° ≈ 44 km² ≈ 4400 ha.
    assert!(
        summary.total_m2 > 30_000_000.0 && summary.total_m2 < 60_000_000.0,
        "área total ~44 km², obtido {:.1} km²",
        summary.total_m2 / 1e6
    );

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// SRS de cálculo é validado: EPSG inexistente → `UnknownSrid` acionável;
/// SRS geográfico (4674, graus) → `GeographicSridForArea` — a área NUNCA é
/// calculada em graus (Decisão 4), para qualquer chamador.
#[tokio::test]
async fn srs_para_area_e_validado() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, _) = make_fixture_grid(&pool, "f5-srs").await;
    let repo = PgAreaRepo::new(pool.clone());

    let err = repo
        .area_summary_on(&grid_id, 999_999)
        .await
        .expect_err("EPSG inexistente é recusado");
    assert!(
        matches!(err, ta_db::Error::UnknownSrid(999_999)),
        "erro acionável de EPSG desconhecido, obtido: {err}"
    );

    let err = repo
        .area_summary_on(&grid_id, 4674)
        .await
        .expect_err("área em graus é recusada");
    assert!(
        matches!(err, ta_db::Error::GeographicSridForArea(4674)),
        "erro de SRS geográfico, obtido: {err}"
    );

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// Por célula: a soma das células é o total, os bloqueios batem e o LIMIT
/// é respeitado (base do export CSV/JSON).
#[tokio::test]
async fn area_por_celula_consistente_com_resumo() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let pool = test_pool().await;
    let (project, grid_id, ids) = make_fixture_grid(&pool, "f5-cells").await;
    block_two(&pool, &grid_id, &ids).await;
    let repo = PgAreaRepo::new(pool.clone());

    let summary = repo.area_summary_on(&grid_id, 5880).await.expect("resumo");
    let by_cell = repo
        .area_by_cell_on(&grid_id, 5880, ta_db::repo_area::MAX_CELLS_EXPORT)
        .await
        .expect("área por célula");

    assert_eq!(by_cell.len() as i64, summary.cells_total);
    let ids_report: BTreeSet<&str> = by_cell.iter().map(|c| c.cell_id.as_str()).collect();
    assert_eq!(ids_report.len(), by_cell.len(), "sem célula repetida");
    let soma: f64 = by_cell.iter().map(|c| c.area_m2).sum();
    assert!(
        (soma - summary.total_m2).abs() <= summary.total_m2.abs() * 1e-6,
        "soma por célula = total (Δ {:e})",
        soma - summary.total_m2
    );
    let bloqueadas = by_cell.iter().filter(|c| c.blocked).count();
    assert_eq!(bloqueadas as i64, summary.cells_blocked);

    let limitado = repo
        .area_by_cell_on(&grid_id, 5880, 5)
        .await
        .expect("export limitado");
    assert_eq!(limitado.len(), 5, "LIMIT é respeitado");

    cleanup(&pool, &project.id).await;
    pool.close().await;
}
