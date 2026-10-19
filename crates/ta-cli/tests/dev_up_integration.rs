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

/// Serializa os testes que apagam/resemeiam o projeto `demo`: o harness roda
/// testes em paralelo no MESMO banco, e um reset concorrente derrubava o
/// projeto no meio do outro teste.
static DEMO_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
///
/// Usa o caminho ORDENADO do produto (`project delete --yes`), nunca um
/// `DELETE FROM projects` cru: `rules.class_id` é `ON DELETE RESTRICT`
/// (0011), então um projeto com ruleset quebra a deleção direta. O caminho
/// ordenado apaga as dependências antes e devolve as contagens.
async fn reset_demo(pool: &PgPool) {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM projects WHERE name = 'demo')")
            .fetch_one(pool)
            .await
            .expect("consultar a existência do projeto demo");
    if !exists {
        return;
    }
    let out = run_cli(&["project", "delete", "--project", "demo", "--yes"]);
    assert!(
        out.status.success(),
        "project delete --yes falhou:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
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
    let _demo = DEMO_LOCK.lock().await;
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

/// Regressão da dívida de teste conhecida: o `reset_demo` antigo fazia
/// `DELETE FROM projects WHERE name='demo'` cru, mas `rules.class_id` é
/// `ON DELETE RESTRICT` (0011). Com um ruleset/régua/task executada, a
/// deleção direta falha e a suíte gated quebrava. O reset pelo caminho
/// ordenado (`project delete --yes`) precisa funcionar nesse estado rico e
/// deixar o banco pronto para o reseed.
#[tokio::test]
async fn reset_funciona_com_projeto_rico_em_rules_e_task() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh --no-tests");
        return;
    }
    let _demo = DEMO_LOCK.lock().await;
    let _ = run_cli(&["migrate", "--to", &dsn()]);
    let pool = PgPool::connect(&dsn())
        .await
        .expect("conectar no PG do harness");
    reset_demo(&pool).await;
    run_seed();

    // Projeto rico: classe → ruleset → regra → task executada (o cenário que
    // o DELETE cru não conseguia apagar por causa do RESTRICT).
    let class = run_cli(&[
        "class",
        "create",
        "--project",
        "demo",
        "--name",
        "classe_reset",
    ]);
    assert!(
        class.status.success(),
        "class create falhou:\n{}",
        String::from_utf8_lossy(&class.stderr)
    );
    let ruleset = run_cli(&[
        "ruleset",
        "create",
        "--project",
        "demo",
        "--name",
        "rs_reset",
    ]);
    assert!(
        ruleset.status.success(),
        "ruleset create falhou:\n{}",
        String::from_utf8_lossy(&ruleset.stderr)
    );

    let ruleset_id: String = sqlx::query_scalar(
        "SELECT rs.id::text FROM rulesets rs JOIN projects p ON p.id = rs.project_id \
         WHERE p.name = 'demo' AND rs.name = 'rs_reset' ORDER BY rs.version DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("id do ruleset de regressão");

    let rule = run_cli(&[
        "rule",
        "add",
        "--ruleset",
        &ruleset_id,
        "--class",
        "classe_reset",
        "--name",
        "r_reset",
        "--when",
        "ndvi >= 0.0",
    ]);
    assert!(
        rule.status.success(),
        "rule add falhou:\n{}",
        String::from_utf8_lossy(&rule.stderr)
    );

    let cell_layer: String = sqlx::query_scalar(
        "SELECT cl.id::text FROM cell_layers cl JOIN projects p ON p.id = cl.project_id \
         WHERE p.name = 'demo' LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("camada de células do demo");

    let activate = run_cli(&[
        "ruleset",
        "activate",
        "--project",
        "demo",
        "--name",
        "rs_reset",
    ]);
    assert!(
        activate.status.success(),
        "ruleset activate falhou:\n{}",
        String::from_utf8_lossy(&activate.stderr)
    );
    let task = run_cli(&[
        "task",
        "create",
        "--project",
        "demo",
        "--cell-layer",
        &cell_layer,
        "classify",
        "--name",
        "t_reset",
    ]);
    assert!(
        task.status.success(),
        "task create falhou:\n{}",
        String::from_utf8_lossy(&task.stderr)
    );

    let task_id: String = sqlx::query_scalar(
        "SELECT t.id::text FROM tasks t JOIN projects p ON p.id = t.project_id \
         WHERE p.name = 'demo' AND t.name = 't_reset' ORDER BY t.created_at DESC LIMIT 1",
    )
    .fetch_one(&pool)
    .await
    .expect("task de regressão");

    let run = run_cli(&["task", "run", &task_id]);
    assert!(
        run.status.success(),
        "task run falhou:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let status: String = sqlx::query_scalar("SELECT status FROM tasks WHERE id = $1::uuid")
        .bind(&task_id)
        .fetch_one(&pool)
        .await
        .expect("status da task de regressão");
    assert_eq!(status, "done", "task de regressão não terminou");
    let logs: i64 =
        sqlx::query_scalar("SELECT count(*)::int8 FROM cell_log WHERE task_id = $1::uuid")
            .bind(&task_id)
            .fetch_one(&pool)
            .await
            .expect("logs da task de regressão");
    assert!(logs > 0, "task executada não gerou log por célula");

    // O reset pelo caminho ordenado precisa apagar o projeto rico (era o
    // RESTRICT de rules.class_id que quebrava o reset antigo).
    reset_demo(&pool).await;
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*)::int8 FROM projects WHERE name = 'demo'")
            .fetch_one(&pool)
            .await
            .expect("contar projetos demo após o reset");
    assert_eq!(remaining, 0, "reset não removeu o projeto demo");

    // E o reseed volta ao estado canônico (2 camadas, 3 classes, 118 células).
    run_seed();
    let after = snapshot(&pool).await;
    assert_eq!(after.0.len(), 2, "reseed após reset: {:?}", after.0);
    assert_eq!(after.1, 3, "reseed após reset: classes");
    assert_eq!(after.2, 1, "reseed após reset: aoi_layers");
    assert_eq!(after.3, 1, "reseed após reset: cell_layers");
    assert_eq!(after.4, 118, "reseed após reset: células");

    pool.close().await;
}
