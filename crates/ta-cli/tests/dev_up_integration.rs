//! Integração do caminho da Fase 0 (`doctor`, `migrate`, `seed-demo`) —
//! **gated** por `TA_TEST_PG=1`, no mesmo harness de `scripts/test-pg.sh`.
//!
//! O crate `ta-cli` é bin-only, então o teste executa o **binário de verdade**
//! (`CARGO_BIN_EXE_ta-cli`): é o que o usuário roda.

use std::path::PathBuf;
use std::process::{Command, Output};

use sqlx::PgPool;

/// DSN padrão = harness de `scripts/test-pg.sh`.
const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

fn dsn() -> String {
    std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned())
}

/// `testdata/` do workspace (o CWD do teste é o crate, não a raiz).
fn testdata(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/monitoring")
        .join(name)
        .display()
        .to_string()
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ta-cli"))
        .args(args)
        .env("TA_PG_DSN", dsn())
        .output()
        .expect("executar o binário ta-cli")
}

fn run_seed() {
    let out = run_cli(&[
        "seed-demo",
        "--aoi",
        &testdata("aoi.geojson"),
        "--points",
        &testdata("points_classes.csv"),
    ]);
    assert!(
        out.status.success(),
        "seed-demo falhou:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Contagens do projeto `demo` no schema REAL da Fase 1:
/// `(features por camada, classes distintas, aoi_layers, cell_layers, cells,
/// features marcadas como provisórias)`.
async fn snapshot(pool: &PgPool) -> (Vec<i64>, i64, i64, i64, i64, i64) {
    let counts: Vec<i64> = sqlx::query_scalar(
        "SELECT count(f.id)::int8 FROM layers l \
         LEFT JOIN features f ON f.layer_id = l.id \
         JOIN projects p ON p.id = l.project_id \
         WHERE p.name = 'demo' GROUP BY l.name ORDER BY l.name",
    )
    .fetch_all(pool)
    .await
    .expect("contagens por camada do projeto demo");

    let classes: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT f.attrs->>'classe')::int8 FROM features f \
         JOIN layers l ON l.id = f.layer_id \
         JOIN projects p ON p.id = l.project_id \
         WHERE p.name = 'demo' AND l.name = 'demo_pontos'",
    )
    .fetch_one(pool)
    .await
    .expect("classes distintas");

    let aoi_layers: i64 = sqlx::query_scalar(
        "SELECT count(*)::int8 FROM aoi_layers a \
         JOIN projects p ON p.id = a.project_id WHERE p.name = 'demo'",
    )
    .fetch_one(pool)
    .await
    .expect("aoi_layers do demo");

    let cell_layers: i64 = sqlx::query_scalar(
        "SELECT count(*)::int8 FROM cell_layers cl \
         JOIN projects p ON p.id = cl.project_id WHERE p.name = 'demo'",
    )
    .fetch_one(pool)
    .await
    .expect("cell_layers do demo");

    let cells: i64 = sqlx::query_scalar(
        "SELECT count(*)::int8 FROM cells c \
         JOIN cell_layers cl ON cl.id = c.cell_layer_id \
         JOIN projects p ON p.id = cl.project_id WHERE p.name = 'demo'",
    )
    .fetch_one(pool)
    .await
    .expect("células da grade real");

    // Regressão: o marcador provisório da Fase 0 não pode voltar.
    let provisional: i64 = sqlx::query_scalar(
        "SELECT count(*)::int8 FROM features f \
         JOIN layers l ON l.id = f.layer_id \
         JOIN projects p ON p.id = l.project_id \
         WHERE p.name = 'demo' AND f.attrs ? 'provisorio'",
    )
    .fetch_one(pool)
    .await
    .expect("features provisórias");

    (counts, classes, aoi_layers, cell_layers, cells, provisional)
}

/// Remove o projeto `demo` para partir de um estado limpo (o seed resolve por
/// nome; remover garante que o teste mede exatamente o que o seed cria).
async fn reset_demo(pool: &PgPool) {
    sqlx::query("DELETE FROM projects WHERE name = 'demo'")
        .execute(pool)
        .await
        .expect("limpar o projeto demo");
}

/// `doctor` reprova (exit != 0) quando o schema não foi aplicado e aprova
/// (exit 0) depois do `migrate` — sem panic nos dois casos.
#[tokio::test]
async fn doctor_aprova_depois_do_migrate() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let migrate = run_cli(&["migrate", "--to", &dsn()]);
    assert!(
        migrate.status.success(),
        "migrate falhou:\n{}",
        String::from_utf8_lossy(&migrate.stderr)
    );

    let out = run_cli(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "doctor reprovou:\n{stdout}");
    assert!(stdout.contains("ambiente pronto"), "saída:\n{stdout}");
    assert!(stdout.contains("migrações"), "saída:\n{stdout}");
}

/// `seed-demo` cria as camadas vetoriais (AOI + pontos), o vínculo de AOI e a
/// grade REAL (`cell_layers` + `cells`); rodar de novo NÃO duplica nada.
#[tokio::test]
async fn seed_demo_cria_o_demo_e_e_idempotente() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let _ = run_cli(&["migrate", "--to", &dsn()]);
    let pool = PgPool::connect(&dsn())
        .await
        .expect("conectar no PG do harness");
    reset_demo(&pool).await;

    run_seed();
    let first = snapshot(&pool).await;
    assert_eq!(
        first.0.len(),
        2,
        "esperava 2 camadas vetoriais do demo (AOI + pontos): {:?}",
        first.0
    );
    assert_eq!(first.1, 3, "esperava 3 classes de exemplo");
    assert_eq!(first.2, 1, "esperava 1 vínculo de AOI");
    assert_eq!(first.3, 1, "esperava 1 camada de células");
    assert_eq!(
        first.4, 118,
        "a grade real do fixture (res 0.005°) tem 118 células"
    );
    assert_eq!(first.5, 0, "nenhuma feature pode ter attrs.provisorio");

    run_seed();
    let second = snapshot(&pool).await;
    assert_eq!(first, second, "seed-demo não é idempotente");

    pool.close().await;
}
