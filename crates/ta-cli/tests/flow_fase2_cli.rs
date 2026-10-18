//! Fluxo de **classes e regras pela CLI** (`class`/`draft`/`ruleset`/`rule`) —
//! **gated** por `TA_TEST_PG=1`, no harness de `scripts/test-pg.sh`.
//!
//! Executa o binário real (`CARGO_BIN_EXE_ta-cli`) e confere o efeito no banco
//! e as mensagens acionáveis do stderr.

use std::process::{Command, Output};

use sqlx::PgPool;
use ta_db::spatial::{is_postgis_ready, run_migrations};

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
        .expect("relógio antes de 1970")
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
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

async fn project_id_by_name(pool: &PgPool, name: &str) -> String {
    sqlx::query_scalar("SELECT id::text FROM projects WHERE name = $1")
        .bind(name)
        .fetch_one(pool)
        .await
        .expect("projeto criado pela CLI")
}

async fn cleanup(pool: &PgPool, project_id: &str) {
    // Com a FK `rules.class_id ON DELETE RESTRICT` (0011), apagar o projeto
    // com regras vivas poderia ser recusado (a cascata pode chegar às classes
    // antes das regras). Tira os rulesets primeiro — eles cascateiam as
    // regras — e depois o projeto.
    sqlx::query("DELETE FROM rulesets WHERE project_id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup de rulesets");
    sqlx::query("DELETE FROM projects WHERE id = $1::uuid")
        .bind(project_id)
        .execute(pool)
        .await
        .expect("cleanup do projeto");
}

#[tokio::test]
async fn cli_crud_de_grupos_classes_e_rascunhos() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("cli_classes");

    run_ok(
        &[
            "class",
            "group",
            "create",
            "--project",
            &project,
            "--name",
            "veg",
            "--sort-order",
            "2",
        ],
        "class group create",
    );
    run_ok(
        &["class", "group", "list", "--project", &project],
        "class group list",
    );
    let project_id = project_id_by_name(&pool, &project).await;

    run_ok(
        &[
            "class",
            "create",
            "--project",
            &project,
            "--name",
            "floresta",
            "--group",
            "veg",
            "--color",
            "#1A7F3C",
        ],
        "class create",
    );
    run_ok(&["class", "list", "--project", &project], "class list");
    let class_id: String = sqlx::query_scalar(
        "SELECT id::text FROM classes WHERE project_id = $1::uuid AND name = 'floresta'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("classe criada");
    let color: Option<String> = sqlx::query_scalar("SELECT color FROM classes WHERE id = $1::uuid")
        .bind(&class_id)
        .fetch_one(&pool)
        .await
        .expect("cor da classe");
    assert_eq!(color.as_deref(), Some("#1A7F3C"));

    run_ok(
        &[
            "class",
            "update",
            "--project",
            &project,
            "--class",
            "floresta",
            "--name",
            "floresta_secundaria",
            "--color",
            "#F2C14E",
        ],
        "class update",
    );
    let updated: (String, Option<String>) =
        sqlx::query_as("SELECT name, color FROM classes WHERE id = $1::uuid")
            .bind(&class_id)
            .fetch_one(&pool)
            .await
            .expect("classe atualizada");
    assert_eq!(updated.0, "floresta_secundaria");
    assert_eq!(updated.1.as_deref(), Some("#F2C14E"));

    run_ok(
        &[
            "class",
            "group",
            "rename",
            "--project",
            &project,
            "--group",
            "veg",
            "--name",
            "vegetacao",
        ],
        "class group rename",
    );
    run_ok(
        &[
            "draft",
            "create",
            "--project",
            &project,
            "--name",
            "rascunho_1",
        ],
        "draft create",
    );
    let state: String = sqlx::query_scalar(
        "SELECT state FROM draft_layers WHERE project_id = $1::uuid AND name = 'rascunho_1'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("rascunho criado");
    assert_eq!(state, "draft");
    run_ok(&["draft", "list", "--project", &project], "draft list");

    assert_cli_fails(
        &[
            "class",
            "create",
            "--project",
            &project,
            "--name",
            "agua",
            "--color",
            "verde",
        ],
        "cor inválida",
    );

    run_ok(
        &[
            "class",
            "group",
            "delete",
            "--project",
            &project,
            "--group",
            "vegetacao",
        ],
        "class group delete",
    );
    let group_after: Option<String> =
        sqlx::query_scalar("SELECT class_group_id::text FROM classes WHERE id = $1::uuid")
            .bind(&class_id)
            .fetch_one(&pool)
            .await
            .expect("grupo desvinculado");
    assert_eq!(group_after, None, "apagar o grupo desvincula a classe");

    run_ok(
        &[
            "class",
            "delete",
            "--project",
            &project,
            "--class",
            "floresta_secundaria",
        ],
        "class delete",
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM classes WHERE project_id = $1::uuid")
            .bind(&project_id)
            .fetch_one(&pool)
            .await
            .expect("conta classes");
    assert_eq!(remaining, 0);

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

#[tokio::test]
async fn cli_ruleset_versiona_e_mostra_versoes() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("cli_ruleset");

    run_ok(
        &[
            "ruleset",
            "create",
            "--project",
            &project,
            "--name",
            "baseline",
        ],
        "ruleset create v1",
    );
    run_ok(
        &[
            "ruleset",
            "create",
            "--project",
            &project,
            "--name",
            "baseline",
            "--description",
            "revisao",
        ],
        "ruleset create v2",
    );
    run_ok(&["ruleset", "list", "--project", &project], "ruleset list");
    run_ok(
        &[
            "ruleset",
            "show",
            "--project",
            &project,
            "--name",
            "baseline",
            "--version",
            "1",
        ],
        "ruleset show v1",
    );

    let project_id = project_id_by_name(&pool, &project).await;
    let mut versions: Vec<i32> = sqlx::query_scalar(
        "SELECT version FROM rulesets WHERE project_id = $1::uuid AND name = 'baseline' \
         ORDER BY version",
    )
    .bind(&project_id)
    .fetch_all(&pool)
    .await
    .expect("versões gravadas");
    versions.sort_unstable();
    assert_eq!(versions, vec![1, 2]);

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

#[tokio::test]
async fn cli_rejeita_predicado_invalido_antes_do_insert() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("cli_predicado");
    run_ok(
        &["ruleset", "create", "--project", &project, "--name", "rs"],
        "ruleset create",
    );
    run_ok(
        &["class", "create", "--project", &project, "--name", "agua"],
        "class create",
    );
    let project_id = project_id_by_name(&pool, &project).await;
    let ruleset_id: String = sqlx::query_scalar(
        "SELECT id::text FROM rulesets WHERE project_id = $1::uuid AND name = 'rs'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("id do ruleset");
    let class_id: String = sqlx::query_scalar(
        "SELECT id::text FROM classes WHERE project_id = $1::uuid AND name = 'agua'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("id da classe");

    let out = run_cli(&[
        "rule",
        "add",
        "--ruleset",
        &ruleset_id,
        "--class",
        &class_id,
        "--name",
        "quebrada",
        "--when",
        "ndvi >=",
    ]);
    assert!(!out.status.success(), "predicado inválido deve falhar");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("predicado inválido") && stderr.contains("incompleta"),
        "mensagem sem causa/posição acionável:\n{stderr}"
    );

    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM rules WHERE ruleset_id = $1::uuid")
        .bind(&ruleset_id)
        .fetch_one(&pool)
        .await
        .expect("conta regras");
    assert_eq!(count, 0, "nada pode ser gravado com predicado inválido");

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

#[tokio::test]
async fn cli_eval_decide_por_prioridade_desempata_por_nome_e_explica_ausentes() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("cli_eval");
    run_ok(
        &["ruleset", "create", "--project", &project, "--name", "rs"],
        "ruleset create",
    );
    run_ok(
        &["class", "create", "--project", &project, "--name", "agua"],
        "class agua",
    );
    run_ok(
        &[
            "class",
            "create",
            "--project",
            &project,
            "--name",
            "floresta",
        ],
        "class floresta",
    );
    let project_id = project_id_by_name(&pool, &project).await;
    let ruleset_id: String = sqlx::query_scalar(
        "SELECT id::text FROM rulesets WHERE project_id = $1::uuid AND name = 'rs'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("id do ruleset");

    run_ok(
        &[
            "rule",
            "add",
            "--ruleset",
            &ruleset_id,
            "--class",
            "agua",
            "--name",
            "agua",
            "--priority",
            "10",
            "--when",
            "ndvi < 0.2",
        ],
        "rule agua",
    );
    // Duas regras empatadas em prioridade 5: o menor nome ("floresta") vence.
    run_ok(
        &[
            "rule",
            "add",
            "--ruleset",
            &ruleset_id,
            "--class",
            "floresta",
            "--name",
            "floresta",
            "--priority",
            "5",
            "--when",
            "ndvi >= 0.2",
        ],
        "rule floresta",
    );
    run_ok(
        &[
            "rule",
            "add",
            "--ruleset",
            &ruleset_id,
            "--class",
            "agua",
            "--name",
            "zebra",
            "--priority",
            "5",
            "--when",
            "ndvi >= 0.2",
        ],
        "rule zebra",
    );
    run_ok(&["rule", "list", "--ruleset", &ruleset_id], "rule list");

    // Prioridade 10 vence as empatadas em 5.
    let out = run_cli(&[
        "rule",
        "eval",
        "--ruleset",
        &ruleset_id,
        "--obs",
        "ndvi=0.1",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("classe=agua") && stdout.contains("regra=agua"),
        "decisão inesperada:\n{stdout}"
    );
    assert!(
        stdout.contains("regras avaliadas: 3"),
        "contagem de avaliadas ausente:\n{stdout}"
    );

    // Empate em 5: "floresta" < "zebra".
    let out = run_cli(&[
        "rule",
        "eval",
        "--ruleset",
        &ruleset_id,
        "--obs",
        "ndvi=0.7",
    ]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("classe=floresta") && stdout.contains("regra=floresta"),
        "desempate pelo menor nome falhou:\n{stdout}"
    );

    // Medida ausente: a regra não casa e a explicação lista a medida.
    let out = run_cli(&[
        "rule",
        "eval",
        "--ruleset",
        &ruleset_id,
        "--obs",
        "swir=0.3",
    ]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("nenhuma classe") && stdout.contains("ndvi"),
        "explicação de medida ausente:\n{stdout}"
    );

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

/// F1 — `class delete` sem `--force` BLOQUEIA mostrando a contagem de
/// regras; com `--force`, regras + classe caem numa única transação.
#[tokio::test]
async fn cli_class_delete_bloqueia_com_contagem_e_force_apaga_atomico() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("cli_classdel");
    run_ok(
        &["ruleset", "create", "--project", &project, "--name", "rs"],
        "ruleset create",
    );
    run_ok(
        &["class", "create", "--project", &project, "--name", "agua"],
        "class create",
    );
    let project_id = project_id_by_name(&pool, &project).await;
    let ruleset_id: String = sqlx::query_scalar(
        "SELECT id::text FROM rulesets WHERE project_id = $1::uuid AND name = 'rs'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("ruleset");

    run_ok(
        &[
            "rule",
            "add",
            "--ruleset",
            &ruleset_id,
            "--class",
            "agua",
            "--name",
            "agua",
            "--when",
            "ndvi < 0.1",
        ],
        "rule add",
    );
    run_ok(
        &[
            "rule",
            "add",
            "--ruleset",
            &ruleset_id,
            "--class",
            "agua",
            "--name",
            "agua2",
            "--when",
            "water == 1",
        ],
        "rule add 2",
    );

    // Sem --force: bloqueia com a contagem e o caminho de saída.
    assert_cli_fails(
        &["class", "delete", "--project", &project, "--class", "agua"],
        "usada por 2 regra(s)",
    );
    let still: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM classes WHERE id = (SELECT id FROM classes \
         WHERE project_id = $1::uuid AND name = 'agua')), \
         (SELECT count(*) FROM rules WHERE ruleset_id = $2::uuid)",
    )
    .bind(&project_id)
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("estado após bloqueio");
    assert_eq!(still, (1, 2), "bloqueio não pode apagar nada");

    // Com --force: as 2 regras e a classe saem na mesma transação.
    run_ok(
        &[
            "class",
            "delete",
            "--project",
            &project,
            "--class",
            "agua",
            "--force",
        ],
        "class delete --force",
    );
    let left: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM classes WHERE project_id = $1::uuid), \
         (SELECT count(*) FROM rules WHERE ruleset_id = $2::uuid)",
    )
    .bind(&project_id)
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("estado após force");
    assert_eq!(left, (0, 0), "force apaga regras + classe atomicamente");

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

/// F2 — 6 processos CLI REAIS criando a MESMA linhagem em paralelo: 6
/// criados, versões {1..6} sem buraco; 5 rodadas para provar estabilidade
/// (lock de linhagem `pg_advisory_xact_lock`).
#[tokio::test]
async fn cli_ruleset_seis_processos_paralelos_em_cinco_rodadas() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("cli_corrida");
    let project_id = {
        // Cria o projeto pela CLI (o primeiro create do round cria o projeto
        // e a linhagem v1 junto; aqui só garantimos o projeto vazio).
        run_ok(
            &["class", "create", "--project", &project, "--name", "ancora"],
            "projeto âncora",
        );
        project_id_by_name(&pool, &project).await
    };

    for round in 1..=5 {
        let lineage = format!("linhagem_{round}");
        let mut handles = Vec::new();
        for worker in 0..6 {
            let lineage = lineage.clone();
            let project = project.clone();
            handles.push(std::thread::spawn(move || {
                run_cli(&[
                    "ruleset",
                    "create",
                    "--project",
                    &project,
                    "--name",
                    &lineage,
                    "--description",
                    &format!("w{worker}"),
                ])
            }));
        }
        let mut failed = Vec::new();
        for out in handles.into_iter().map(|h| h.join().expect("thread")) {
            if !out.status.success() {
                failed.push(String::from_utf8_lossy(&out.stderr).into_owned());
            }
        }
        assert!(
            failed.is_empty(),
            "rodada {round}: {}/6 processos falharam:\n{}",
            failed.len(),
            failed.join("\n---\n")
        );
        let mut versions: Vec<i32> = sqlx::query_scalar(
            "SELECT version FROM rulesets WHERE project_id = $1::uuid AND name = $2 \
             ORDER BY version",
        )
        .bind(&project_id)
        .bind(&lineage)
        .fetch_all(&pool)
        .await
        .expect("versões da rodada");
        versions.sort_unstable();
        assert_eq!(
            versions,
            vec![1, 2, 3, 4, 5, 6],
            "rodada {round}: versões sem buraco e sem repetição"
        );
    }

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

/// F3 — regra com `NOT NDVI >= 1` e célula SEM `ndvi` vira "nenhuma classe"
/// (célula sem observação não é classificada por negação); F4 — regra com
/// predicado inválido no banco sai contada e nomeada no eval.
#[tokio::test]
async fn cli_eval_not_sem_medida_e_regras_invalidas_tem_rastro() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("cli_not");
    run_ok(
        &["ruleset", "create", "--project", &project, "--name", "rs"],
        "ruleset create",
    );
    run_ok(
        &[
            "class",
            "create",
            "--project",
            &project,
            "--name",
            "naoagua",
        ],
        "class create",
    );
    let project_id = project_id_by_name(&pool, &project).await;
    let ruleset_id: String = sqlx::query_scalar(
        "SELECT id::text FROM rulesets WHERE project_id = $1::uuid AND name = 'rs'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("ruleset");
    run_ok(
        &[
            "rule",
            "add",
            "--ruleset",
            &ruleset_id,
            "--class",
            "naoagua",
            "--name",
            "negada",
            "--when",
            "not ndvi >= 1",
        ],
        "rule add not",
    );

    // Sem ndvi: a regra negada NUNCA casa → nenhuma classe, com a medida
    // ausente explicada.
    let out = run_cli(&[
        "rule",
        "eval",
        "--ruleset",
        &ruleset_id,
        "--obs",
        "nbr=0.05",
    ]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("nenhuma classe") && stdout.contains("ndvi"),
        "`not` com medida ausente classificou sem dados:\n{stdout}"
    );
    assert!(
        !stdout.contains("regras ignoradas"),
        "sem inválidas, não há rastro de ignoradas:\n{stdout}"
    );

    // Com ndvi presente e baixo: a negação casa normalmente.
    let out = run_cli(&[
        "rule",
        "eval",
        "--ruleset",
        &ruleset_id,
        "--obs",
        "ndvi=0.2",
    ]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("classe=naoagua"),
        "`not` com medida presente deve casar:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // F4 — predicado inválido direto no banco (a CLI valida na escrita):
    // o eval não cai e mostra o rastro.
    sqlx::query(
        "INSERT INTO rules (ruleset_id, class_id, name, predicate) \
                 VALUES ($1::uuid, (SELECT id FROM classes WHERE project_id = $2::uuid \
                 AND name = 'naoagua'), 'quebrada', 'ndvi >>>')",
    )
    .bind(&ruleset_id)
    .bind(&project_id)
    .execute(&pool)
    .await
    .expect("regra inválida no banco");
    let out = run_cli(&[
        "rule",
        "eval",
        "--ruleset",
        &ruleset_id,
        "--obs",
        "ndvi=0.2",
    ]);
    assert!(out.status.success(), "inválida não derruba o eval");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("regras ignoradas (predicado inválido): 1 [quebrada]"),
        "rastro de ignoradas ausente:\n{stdout}"
    );

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

/// F8 — `rule update` parcial (só as flags presentes mudam) e
/// `class update --active` (atributo de exibição).
#[tokio::test]
async fn cli_rule_update_parcial_e_class_update_active() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("cli_update");
    run_ok(
        &["ruleset", "create", "--project", &project, "--name", "rs"],
        "ruleset create",
    );
    run_ok(
        &["class", "create", "--project", &project, "--name", "agua"],
        "class agua",
    );
    run_ok(
        &["class", "create", "--project", &project, "--name", "solo"],
        "class solo",
    );
    let project_id = project_id_by_name(&pool, &project).await;
    let ruleset_id: String = sqlx::query_scalar(
        "SELECT id::text FROM rulesets WHERE project_id = $1::uuid AND name = 'rs'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("ruleset");
    run_ok(
        &[
            "rule",
            "add",
            "--ruleset",
            &ruleset_id,
            "--class",
            "agua",
            "--name",
            "agua",
            "--priority",
            "3",
            "--when",
            "ndvi < 0.1",
        ],
        "rule add",
    );

    // Sem flags: erro acionável.
    assert_cli_fails(
        &["rule", "update", "--ruleset", &ruleset_id, "--name", "agua"],
        "nada para atualizar",
    );
    // Predicado inválido é barrado antes do banco.
    assert_cli_fails(
        &[
            "rule",
            "update",
            "--ruleset",
            &ruleset_id,
            "--name",
            "agua",
            "--when",
            "ndvi >=",
        ],
        "predicado inválido",
    );

    // Só --active: prioridade/predicado/classe preservados.
    run_ok(
        &[
            "rule",
            "update",
            "--ruleset",
            &ruleset_id,
            "--name",
            "agua",
            "--active",
            "false",
        ],
        "rule update active",
    );
    let after: (bool, i32, String, String) = sqlx::query_as(
        "SELECT is_active, priority, predicate, class_id::text FROM rules \
         WHERE ruleset_id = $1::uuid AND name = 'agua'",
    )
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("regra após update");
    assert!(!after.0);
    assert_eq!(after.1, 3, "prioridade preservada");
    assert_eq!(after.2, "ndvi < 0.1", "predicado preservado");
    let agua_id: String = sqlx::query_scalar(
        "SELECT id::text FROM classes WHERE project_id = $1::uuid AND name = 'agua'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("classe agua");
    assert_eq!(after.3, agua_id, "classe preservada");

    // Só --priority e --class juntos; --when ausente preserva.
    run_ok(
        &[
            "rule",
            "update",
            "--ruleset",
            &ruleset_id,
            "--name",
            "agua",
            "--priority",
            "9",
            "--class",
            "solo",
            "--active",
            "true",
        ],
        "rule update prioridade+classe",
    );
    let after: (bool, i32, String) = sqlx::query_as(
        "SELECT is_active, priority, predicate FROM rules \
         WHERE ruleset_id = $1::uuid AND name = 'agua'",
    )
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("regra após update 2");
    assert_eq!(
        (after.0, after.1, after.2.as_str()),
        (true, 9, "ndvi < 0.1")
    );
    let solo_id: String = sqlx::query_scalar(
        "SELECT id::text FROM classes WHERE project_id = $1::uuid AND name = 'solo'",
    )
    .bind(&project_id)
    .fetch_one(&pool)
    .await
    .expect("classe solo");
    let class_id: String = sqlx::query_scalar(
        "SELECT class_id::text FROM rules WHERE ruleset_id = $1::uuid AND name = 'agua'",
    )
    .bind(&ruleset_id)
    .fetch_one(&pool)
    .await
    .expect("classe da regra");
    assert_eq!(class_id, solo_id);

    // class update --active (exibição).
    run_ok(
        &[
            "class",
            "update",
            "--project",
            &project,
            "--class",
            "solo",
            "--active",
            "false",
        ],
        "class update active",
    );
    let active: bool = sqlx::query_scalar("SELECT is_active FROM classes WHERE id = $1::uuid")
        .bind(&solo_id)
        .fetch_one(&pool)
        .await
        .expect("is_active da classe");
    assert!(!active);

    cleanup(&pool, &project_id).await;
    pool.close().await;
}

/// F12 — `--project` vazio/só-espaços é rejeitado com erro acionável
/// (não auto-cria projeto sem nome).
#[tokio::test]
async fn cli_rejeita_projeto_vazio() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let _pool = migrate_and_pool().await;
    for vazio in ["", "   "] {
        assert_cli_fails(
            &["class", "list", "--project", vazio],
            "nome de projeto vazio",
        );
    }
    // Nada foi criado: nenhum projeto com nome vazio/só-espaços.
    let empty_named: i64 =
        sqlx::query_scalar("SELECT count(*) FROM projects WHERE btrim(name) = ''")
            .fetch_one(&_pool)
            .await
            .expect("projetos sem nome");
    assert_eq!(empty_named, 0, "projeto sem nome não pode existir");
    _pool.close().await;
}

#[tokio::test]
async fn cli_rejeita_classe_de_outro_projeto_no_rule_add() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = migrate_and_pool().await;
    let project_a = unique_name("cli_proj_a");
    let project_b = unique_name("cli_proj_b");
    run_ok(
        &["ruleset", "create", "--project", &project_a, "--name", "rs"],
        "ruleset do projeto A",
    );
    run_ok(
        &["class", "create", "--project", &project_b, "--name", "agua"],
        "classe do projeto B",
    );
    let project_a_id = project_id_by_name(&pool, &project_a).await;
    let project_b_id = project_id_by_name(&pool, &project_b).await;
    let ruleset_id: String = sqlx::query_scalar(
        "SELECT id::text FROM rulesets WHERE project_id = $1::uuid AND name = 'rs'",
    )
    .bind(&project_a_id)
    .fetch_one(&pool)
    .await
    .expect("ruleset A");
    let foreign_class: String = sqlx::query_scalar(
        "SELECT id::text FROM classes WHERE project_id = $1::uuid AND name = 'agua'",
    )
    .bind(&project_b_id)
    .fetch_one(&pool)
    .await
    .expect("classe B");

    assert_cli_fails(
        &[
            "rule",
            "add",
            "--ruleset",
            &ruleset_id,
            "--class",
            &foreign_class,
            "--name",
            "cruzada",
            "--when",
            "ndvi < 0.1",
        ],
        "pertence a outro projeto",
    );

    cleanup(&pool, &project_a_id).await;
    cleanup(&pool, &project_b_id).await;
    pool.close().await;
}
