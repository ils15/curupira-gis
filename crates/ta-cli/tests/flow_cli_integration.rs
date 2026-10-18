//! Fluxo de monitoramento pela **CLI** (`phase`/`aoi`/`grid`/`cell`) — **gated**
//! por `TA_TEST_PG=1`, no harness de `scripts/test-pg.sh`.
//!
//! O crate `ta-cli` é bin-only: o teste executa o **binário de verdade**
//! (`CARGO_BIN_EXE_ta-cli`), como o usuário roda. A montagem do projeto/camada
//! é feita direto no banco (ta-core/ta-db) para isolar o que é da CLI; as
//! asserções olham o efeito no banco e as mensagens acionáveis do stderr.

use std::process::{Command, Output};

use geo_types::{LineString, Polygon};
use sqlx::PgPool;
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::spatial::{TARGET_SRID, is_postgis_ready, run_migrations};

const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

fn dsn() -> String {
    std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned())
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ta-cli"))
        .args(args)
        .env("TA_PG_DSN", dsn())
        .output()
        .expect("executar o binário ta-cli")
}

fn run_ok(args: &[&str], ctx: &str) {
    let out = run_cli(args);
    assert!(
        out.status.success(),
        "{ctx} falhou:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Roda a CLI esperando falha e confere que o stderr traz a mensagem acionável.
fn assert_cli_fails(args: &[&str], needle: &str) {
    let out = run_cli(args);
    assert!(
        !out.status.success(),
        "esperava falha, mas passou:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(needle),
        "stderr sem a mensagem `{needle}`:\n{stderr}"
    );
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

async fn migrate_and_pool() -> PgPool {
    run_ok(&["migrate", "--to", &dsn()], "migrate");
    let pool = PgPool::connect(&dsn())
        .await
        .expect("conectar no PG do harness");
    if !is_postgis_ready(&pool).await.expect("pg_extension") {
        ta_db::spatial::ensure_postgis(&pool)
            .await
            .expect("PostGIS no harness");
    }
    run_migrations(&pool).await.expect("migrações aplicam");
    pool
}

/// Projeto + camada poligonal com 1 feição (a geometria da AOI). Devolve
/// `(project_id, layer_id)`.
async fn setup_project_layer(pool: &PgPool, prefix: &str) -> (String, String) {
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
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("aoi"),
            geom_type: GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "test://flow".into(),
        })
        .await
        .expect("cria camada");
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(
                rect(-52.20, -3.20, -52.14, -3.16).into(),
                serde_json::json!({}),
            )],
        )
        .await
        .expect("insere a AOI");
    (project.id, layer.id)
}

async fn cleanup(pool: &PgPool, project_id: &str) {
    sqlx::query("DELETE FROM projects WHERE id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup do projeto");
}

#[tokio::test]
async fn fluxo_cli_cria_fase_aoi_grade_e_bloqueia_celula() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, layer_id) = setup_project_layer(&pool, "flow").await;

    // phase create
    run_ok(
        &[
            "phase",
            "create",
            "--project",
            &project_id,
            "--name",
            "f1",
            "--cell-blocker",
        ],
        "phase create",
    );
    let phase_id: String = sqlx::query_scalar(
        "SELECT id::text FROM phases WHERE project_id = $1::uuid AND name = 'f1'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("id da fase");

    // aoi create --phase (fase do mesmo projeto)
    run_ok(
        &[
            "aoi",
            "create",
            "--project",
            &project_id,
            "--layer",
            &layer_id,
            "--phase",
            &phase_id,
        ],
        "aoi create",
    );
    let aoi_id: String =
        sqlx::query_scalar("SELECT id::text FROM aoi_layers WHERE project_id = $1::uuid")
            .bind(&project_id)
            .fetch_one(&pool)
            .await
            .expect("id da AOI");
    let aoi_phase: Option<String> =
        sqlx::query_scalar("SELECT phase_id::text FROM aoi_layers WHERE id = $1::uuid")
            .bind(&aoi_id)
            .fetch_one(&pool)
            .await
            .expect("phase da AOI");
    assert_eq!(aoi_phase.as_deref(), Some(phase_id.as_str()));

    // grid create --phase
    run_ok(
        &[
            "grid",
            "create",
            "--aoi-layer",
            &aoi_id,
            "--resolution",
            "0.005",
            "--name",
            "g1",
            "--phase",
            &phase_id,
        ],
        "grid create",
    );
    let grid_id: String =
        sqlx::query_scalar("SELECT id::text FROM cell_layers WHERE project_id = $1::uuid")
            .bind(&project_id)
            .fetch_one(&pool)
            .await
            .expect("id da grade");
    let grid_phase: Option<String> =
        sqlx::query_scalar("SELECT phase_id::text FROM cell_layers WHERE id = $1::uuid")
            .bind(&grid_id)
            .fetch_one(&pool)
            .await
            .expect("phase da grade");
    assert_eq!(grid_phase.as_deref(), Some(phase_id.as_str()));
    // grid info rotula o SRS da ORIGEM (unidades de resolução/origem) e o da
    // extensão (armazenamento), sem misturar os dois sem rótulo.
    let out = run_cli(&["grid", "info", "--cell-layer", &grid_id]);
    assert!(out.status.success(), "grid info falhou");
    let info = String::from_utf8_lossy(&out.stdout);
    assert!(
        info.contains("origem=(") && info.contains("EPSG:4674"),
        "grid info sem SRS rotulado na origem: {info}"
    );
    assert!(
        info.contains("extensão (EPSG:4674, armazenamento)"),
        "grid info sem rótulo de armazenamento na extensão: {info}"
    );

    let n: i64 =
        sqlx::query_scalar("SELECT count(*)::int8 FROM cells WHERE cell_layer_id = $1::uuid")
            .bind(&grid_id)
            .fetch_one(&pool)
            .await
            .expect("contagem de células");
    assert!(n > 0, "a grade deveria ter células");

    // cell block + list-blocked
    let cell_id: String = sqlx::query_scalar(
        "SELECT cell_id FROM cells WHERE cell_layer_id = $1::uuid ORDER BY cell_id LIMIT 1",
    )
    .bind(&grid_id)
    .fetch_one(&pool)
    .await
    .expect("primeira célula");
    run_ok(
        &[
            "cell",
            "block",
            "--cell-layer",
            &grid_id,
            "--cell-id",
            &cell_id,
            "--reason",
            "teste",
        ],
        "cell block",
    );
    let out = run_cli(&["cell", "list-blocked", "--cell-layer", &grid_id]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&cell_id),
        "list-blocked não mostrou a célula {cell_id}"
    );

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

#[tokio::test]
async fn fase_de_outro_projeto_da_erro_acionavel() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_a, _layer_a) = setup_project_layer(&pool, "flowA").await;
    let (project_b, layer_b) = setup_project_layer(&pool, "flowB").await;

    run_ok(
        &["phase", "create", "--project", &project_a, "--name", "fa"],
        "phase create",
    );
    let phase_a: String =
        sqlx::query_scalar("SELECT id::text FROM phases WHERE project_id = $1::uuid")
            .bind(&project_a)
            .fetch_one(&pool)
            .await
            .expect("id da fase");

    // AOI do projeto B com uma fase do projeto A → erro acionável.
    assert_cli_fails(
        &[
            "aoi",
            "create",
            "--project",
            &project_b,
            "--layer",
            &layer_b,
            "--phase",
            &phase_a,
        ],
        "pertence a outro projeto",
    );

    cleanup(&pool, &project_a).await;
    cleanup(&pool, &project_b).await;
    pool.close().await;
}

#[tokio::test]
async fn fase_nome_duplicado_da_erro_acionavel() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, _layer) = setup_project_layer(&pool, "flow").await;

    run_ok(
        &["phase", "create", "--project", &project_id, "--name", "dup"],
        "1ª fase",
    );
    assert_cli_fails(
        &["phase", "create", "--project", &project_id, "--name", "dup"],
        "já existe uma fase",
    );

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

#[tokio::test]
async fn grid_nome_duplicado_da_erro_acionavel() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, layer_id) = setup_project_layer(&pool, "flow").await;
    run_ok(
        &[
            "aoi",
            "create",
            "--project",
            &project_id,
            "--layer",
            &layer_id,
        ],
        "aoi create",
    );
    let aoi_id: String =
        sqlx::query_scalar("SELECT id::text FROM aoi_layers WHERE project_id = $1::uuid")
            .bind(&project_id)
            .fetch_one(&pool)
            .await
            .expect("id da AOI");

    run_ok(
        &[
            "grid",
            "create",
            "--aoi-layer",
            &aoi_id,
            "--resolution",
            "0.005",
            "--name",
            "g",
        ],
        "1ª grade",
    );
    assert_cli_fails(
        &[
            "grid",
            "create",
            "--aoi-layer",
            &aoi_id,
            "--resolution",
            "0.005",
            "--name",
            "g",
        ],
        "já existe uma camada de células",
    );

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

#[tokio::test]
async fn grid_resolucao_maior_que_a_aoi_da_erro_acionavel() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, layer_id) = setup_project_layer(&pool, "flow").await;
    run_ok(
        &[
            "aoi",
            "create",
            "--project",
            &project_id,
            "--layer",
            &layer_id,
        ],
        "aoi create",
    );
    let aoi_id: String =
        sqlx::query_scalar("SELECT id::text FROM aoi_layers WHERE project_id = $1::uuid")
            .bind(&project_id)
            .fetch_one(&pool)
            .await
            .expect("id da AOI");

    // Resolução 100° sobre uma AOI de ~0.06° → nenhuma célula → erro acionável.
    // Inclui origens NEGATIVAS: elas precisam ser parseadas como valor (não
    // como flag desconhecida do clap) e chegar à mesma validação.
    assert_cli_fails(
        &[
            "grid",
            "create",
            "--aoi-layer",
            &aoi_id,
            "--resolution",
            "100",
            "--origin-x",
            "-52.2",
            "--origin-y",
            "-3.2",
        ],
        "nenhuma célula gerada",
    );

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

/// EPSG inexistente em `--srid` → erro acionável (não panic, não stack de banco
/// cru). A validação consulta `spatial_ref_sys` antes de qualquer reprojeção.
#[tokio::test]
async fn grid_srid_inexistente_da_erro_acionavel() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let (project_id, layer_id) = setup_project_layer(&pool, "flow").await;
    run_ok(
        &[
            "aoi",
            "create",
            "--project",
            &project_id,
            "--layer",
            &layer_id,
        ],
        "aoi create",
    );
    let aoi_id: String =
        sqlx::query_scalar("SELECT id::text FROM aoi_layers WHERE project_id = $1::uuid")
            .bind(&project_id)
            .fetch_one(&pool)
            .await
            .expect("id da AOI");

    assert_cli_fails(
        &[
            "grid",
            "create",
            "--aoi-layer",
            &aoi_id,
            "--resolution",
            "500",
            "--srid",
            "999999",
        ],
        "EPSG 999999 desconhecido",
    );

    // `-1` nos DOIS formatos (`--srid -1` e `--srid=-1`) precisa chegar à
    // validação acionável — não virar "flag desconhecida" do clap.
    assert_cli_fails(
        &[
            "grid",
            "create",
            "--aoi-layer",
            &aoi_id,
            "--resolution",
            "500",
            "--srid",
            "-1",
        ],
        "EPSG -1 desconhecido",
    );
    assert_cli_fails(
        &[
            "grid",
            "create",
            "--aoi-layer",
            &aoi_id,
            "--resolution",
            "500",
            "--srid=-1",
        ],
        "EPSG -1 desconhecido",
    );

    cleanup(&pool, &project_id).await;
    pool.close().await;
}
