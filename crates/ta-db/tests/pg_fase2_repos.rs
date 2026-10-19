//! Repositórios da **Fase 2** (classes/grupos/rascunhos e rulesets/regras) —
//! **gated** por `TA_TEST_PG=1`.
//!
//! Cluster de referência: `scripts/test-pg.sh` (PG16 + PostGIS 3.4, porta
//! 55432); DSN configurável por `TA_PG_DSN`. Sem a variável, cada teste
//! imprime "skipped" e passa.

use sqlx::PgPool;
use ta_core::classification::{Class, ClassGroup, DraftLayer, DraftState};
use ta_core::project::Project;
use ta_core::rule::{Rule, RuleSet};
use ta_db::Error as DbError;
use ta_db::repo::{PgProjectRepo, ProjectRepo};
use ta_db::repo_class::{
    ClassGroupRepo, ClassRepo, DraftLayerRepo, PgClassGroupRepo, PgClassRepo, PgDraftLayerRepo,
};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RulePatch, RuleRepo, RuleSetRepo};
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
        .expect("relógio antes de 1970")
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
}

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
    PgProjectRepo::new(pool.clone())
        .delete(project_id)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn class_group_e_class_crud_completo() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase2").await;
    let groups = PgClassGroupRepo::new(pool.clone());
    let classes = PgClassRepo::new(pool.clone());

    let mut group = ClassGroup::new(&project.id, "vegetacao");
    group.sort_order = 2;
    let saved_group = groups.create(&group).await.expect("cria grupo");
    assert_eq!(saved_group.project_id, project.id);
    assert_eq!(
        groups
            .list_for_project(&project.id)
            .await
            .expect("lista")
            .len(),
        1
    );

    group.id = saved_group.id.clone();
    group.name = "vegetacao_densa".into();
    group.sort_order = 3;
    let renamed = groups
        .update(&group)
        .await
        .expect("renomeia")
        .expect("grupo existe");
    assert_eq!(renamed.name, "vegetacao_densa");

    let parent = classes
        .create(&Class::new(&project.id, "floresta"))
        .await
        .expect("cria classe-mãe");
    let child = classes
        .create(
            &Class::new(&project.id, "floresta_primaria")
                .with_group(Some(&saved_group.id))
                .with_parent(Some(&parent.id))
                .with_color(Some("#1A7F3C")),
        )
        .await
        .expect("cria classe");
    assert_eq!(
        child.class_group_id.as_deref(),
        Some(saved_group.id.as_str())
    );
    assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(child.color.as_deref(), Some("#1A7F3C"));
    assert!(child.is_active);

    let fetched = classes.get(&child.id).await.expect("get").expect("existe");
    assert_eq!(fetched.name, "floresta_primaria");
    assert_eq!(
        classes
            .list_for_project(&project.id)
            .await
            .expect("lista")
            .len(),
        2
    );

    let mut updated = fetched;
    updated.name = "floresta_secundaria".into();
    updated.color = Some("#F2C14E".into());
    updated.is_active = false;
    let updated = classes
        .update(&updated)
        .await
        .expect("atualiza")
        .expect("classe existe");
    assert_eq!(updated.name, "floresta_secundaria");
    assert_eq!(updated.color.as_deref(), Some("#F2C14E"));
    assert!(!updated.is_active);

    assert!(classes.delete(&child.id).await.expect("remove classe"));
    assert!(classes.get(&child.id).await.expect("get").is_none());
    assert!(groups.delete(&saved_group.id).await.expect("remove grupo"));
    assert!(groups.get(&saved_group.id).await.expect("get").is_none());

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

#[tokio::test]
async fn draft_layer_cria_lista_e_cai_com_o_projeto() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase2").await;
    let repo = PgDraftLayerRepo::new(pool.clone());

    let saved = repo
        .create(&DraftLayer::new(&project.id, "rascunho_1"))
        .await
        .expect("cria rascunho");
    assert_eq!(saved.state, DraftState::Draft);
    assert_eq!(saved.phase_id, None);
    assert_eq!(saved.cell_layer_id, None);

    let listed = repo.list_for_project(&project.id).await.expect("lista");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, saved.id);

    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM draft_layers WHERE project_id = $1::uuid")
            .bind(&project.id)
            .fetch_one(&pool)
            .await
            .expect("conta rascunhos");
    assert_eq!(count, 1);

    cleanup(&pool, &project.id).await;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM draft_layers WHERE project_id = $1::uuid")
            .bind(&project.id)
            .fetch_one(&pool)
            .await
            .expect("conta rascunhos pós-cleanup");
    assert_eq!(count, 0, "projeto removido leva o rascunho em cascata");
    pool.close().await;
}

#[tokio::test]
async fn ruleset_versiona_por_nome_e_versao_e_unica() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase2").await;
    let repo = PgRuleSetRepo::new(pool.clone());

    let v1 = repo
        .create(&RuleSet::new(&project.id, "baseline"))
        .await
        .expect("v1");
    let v2 = repo
        .create(&RuleSet::new(&project.id, "baseline").with_description(Some("revisao")))
        .await
        .expect("v2");
    assert_eq!(v1.version, 1);
    assert_eq!(v2.version, 2);

    let latest = repo
        .find(&project.id, "baseline", None)
        .await
        .expect("find")
        .expect("existe");
    assert_eq!(latest.version, 2);
    assert_eq!(latest.description.as_deref(), Some("revisao"));
    let first = repo
        .find(&project.id, "baseline", Some(1))
        .await
        .expect("find v1")
        .expect("v1 existe");
    assert_eq!(first.id, v1.id);

    let mut latest = latest;
    latest.description = Some("ajustada".into());
    let latest = repo
        .update(&latest)
        .await
        .expect("atualiza")
        .expect("ruleset existe");
    assert_eq!(latest.description.as_deref(), Some("ajustada"));
    assert_eq!(
        repo.list_for_project(&project.id)
            .await
            .expect("lista")
            .len(),
        2
    );

    // O mesmo (projeto, nome, versão) não pode existir duas vezes: o banco é
    // a garantia contra corrida entre escritas concorrentes.
    let duplicate = sqlx::query(
        "INSERT INTO rulesets (project_id, name, version) VALUES ($1::uuid, 'baseline', 1)",
    )
    .bind(&project.id)
    .execute(&pool)
    .await
    .expect_err("versão repetida deve violar UNIQUE");
    match duplicate {
        sqlx::Error::Database(db) => assert!(db.is_unique_violation()),
        other => panic!("esperava violação de unicidade, veio {other:?}"),
    }

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

#[tokio::test]
async fn rule_exige_classe_do_mesmo_projeto_e_ruleset_remove_em_cascata() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase2").await;
    let other = new_project(&pool, "fase2").await;
    let class = PgClassRepo::new(pool.clone())
        .create(&Class::new(&project.id, "agua"))
        .await
        .expect("classe do projeto");
    let foreign_class = PgClassRepo::new(pool.clone())
        .create(&Class::new(&other.id, "agua"))
        .await
        .expect("classe de outro projeto");
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(&project.id, "classico"))
        .await
        .expect("ruleset");
    let rules = PgRuleRepo::new(pool.clone());

    let saved = rules
        .create(&Rule::new(&ruleset.id, &class.id, "agua", "ndvi < 0.1"))
        .await
        .expect("cria regra");
    assert_eq!(saved.class_id, class.id);

    let err = rules
        .create(&Rule::new(
            &ruleset.id,
            &foreign_class.id,
            "outra",
            "ndvi < 0.1",
        ))
        .await
        .expect_err("classe de outro projeto deve ser rejeitada");
    assert!(
        matches!(err, DbError::ClassProjectMismatch { .. }),
        "erro tipado esperado, veio {err}"
    );
    assert!(
        err.to_string().contains("pertence ao projeto"),
        "mensagem acionável ausente: {err}"
    );

    let mut updated = saved.clone();
    updated.priority = 7;
    updated.predicate = "ndvi < 0.05".into();
    let updated = rules
        .update(&updated)
        .await
        .expect("atualiza")
        .expect("regra existe");
    assert_eq!(updated.priority, 7);
    assert_eq!(
        rules
            .list_for_ruleset(&ruleset.id)
            .await
            .expect("lista")
            .len(),
        1
    );

    assert!(rules.delete(&updated.id).await.expect("remove regra"));
    assert!(rules.get(&updated.id).await.expect("get").is_none());

    // Recria e apaga o ruleset: a regra cai em cascata.
    rules
        .create(&Rule::new(&ruleset.id, &class.id, "agua", "ndvi < 0.1"))
        .await
        .expect("recria regra");
    assert!(
        PgRuleSetRepo::new(pool.clone())
            .delete(&ruleset.id)
            .await
            .expect("remove ruleset")
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rules WHERE ruleset_id = $1::uuid")
            .bind(&ruleset.id)
            .fetch_one(&pool)
            .await
            .expect("conta regras");
    assert_eq!(remaining, 0, "ruleset removido leva as regras em cascata");

    cleanup(&pool, &project.id).await;
    cleanup(&pool, &other.id).await;
    pool.close().await;
}

#[tokio::test]
async fn versao_concorrente_nao_colide() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase2").await;
    let name = unique_name("concorrente");
    let a = RuleSet::new(&project.id, &name);
    let b = RuleSet::new(&project.id, &name);

    let repo_a = PgRuleSetRepo::new(pool.clone());
    let repo_b = PgRuleSetRepo::new(pool.clone());
    let (first, second) = tokio::join!(repo_a.create(&a), repo_b.create(&b));
    let mut versions = vec![
        first.expect("primeira escrita").version,
        second.expect("segunda escrita").version,
    ];
    versions.sort_unstable();
    assert_eq!(
        versions,
        vec![1, 2],
        "as duas escritas viram versões distintas"
    );

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// F1 — apagar classe com regra vira erro tipado acionável (FK RESTRICT da
/// 0011) e o caminho `--force` (`delete_with_rules`) apaga regras + classe
/// numa única transação.
#[tokio::test]
async fn classe_com_regra_recusa_delete_e_force_apaga_atomico() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase2").await;
    let classes = PgClassRepo::new(pool.clone());
    let rules = PgRuleRepo::new(pool.clone());

    let class = classes
        .create(&Class::new(&project.id, "agua"))
        .await
        .expect("cria classe");
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(&project.id, "rs"))
        .await
        .expect("cria ruleset");
    rules
        .create(&Rule::new(&ruleset.id, &class.id, "agua", "ndvi < 0.1"))
        .await
        .expect("cria regra");

    // Sem force: FK RESTRICT → erro tipado com contagem e caminho de saída.
    let err = classes
        .delete(&class.id)
        .await
        .expect_err("delete com regra deve ser recusado");
    match &err {
        DbError::ClassInUseByRules { class_id, count } => {
            assert_eq!(class_id, &class.id);
            assert_eq!(*count, 1, "contagem de regras no erro");
        }
        other => panic!("esperava ClassInUseByRules, veio {other:?}"),
    }
    assert!(
        err.to_string().contains("1 regra(s)") && err.to_string().contains("--force"),
        "mensagem acionável ausente: {err}"
    );
    // Nada foi apagado.
    assert!(classes.get(&class.id).await.expect("get").is_some());
    assert_eq!(rules.count_for_class(&class.id).await.expect("count"), 1);

    // Com force (transação única): regras e classe somem juntas.
    assert!(classes.delete_with_rules(&class.id).await.expect("force"));
    assert!(classes.get(&class.id).await.expect("get").is_none());
    assert_eq!(rules.count_for_class(&class.id).await.expect("count"), 0);

    // Classe SEM regras: delete direto continua funcionando.
    let solta = classes
        .create(&Class::new(&project.id, "solo"))
        .await
        .expect("classe sem regras");
    assert!(classes.delete(&solta.id).await.expect("delete direto"));

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// F6 — ciclo de `parent_id` é bloqueado na escrita: A→B→A e auto-pai são
/// rejeitados com erro tipado acionável.
#[tokio::test]
async fn parent_id_ciclo_e_auto_pai_sao_rejeitados() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase2").await;
    let classes = PgClassRepo::new(pool.clone());

    let a = classes
        .create(&Class::new(&project.id, "vegetacao"))
        .await
        .expect("cria A");
    let b = classes
        .create(&Class::new(&project.id, "floresta").with_parent(Some(&a.id)))
        .await
        .expect("cria B filha de A");

    // Auto-pai.
    let mut a_self = a.clone();
    a_self.parent_id = Some(a.id.clone());
    let err = classes
        .update(&a_self)
        .await
        .expect_err("auto-pai deve ser rejeitado");
    assert!(
        matches!(err, DbError::ClassParentCycle { .. }),
        "erro tipado esperado, veio {err}"
    );

    // A→B→A: apontar a mãe de A para B fecha o ciclo.
    let mut a_ciclo = a.clone();
    a_ciclo.parent_id = Some(b.id.clone());
    let err = classes
        .update(&a_ciclo)
        .await
        .expect_err("ciclo A→B→A deve ser rejeitado");
    match &err {
        DbError::ClassParentCycle {
            class_id,
            parent_id,
        } => {
            assert_eq!(class_id, &a.id);
            assert_eq!(parent_id, &b.id);
        }
        other => panic!("esperava ClassParentCycle, veio {other:?}"),
    }
    assert!(
        err.to_string().contains("ciclo"),
        "mensagem acionável ausente: {err}"
    );
    // O estado no banco não mudou (A continua sem mãe).
    assert_eq!(
        classes
            .get(&a.id)
            .await
            .expect("get")
            .expect("A existe")
            .parent_id,
        None,
        "update recusado não pode ter alterado a classe"
    );

    // Cadeia sem ciclo segue aceita: A→B já existe; neta C→B é válida.
    let c = classes
        .create(&Class::new(&project.id, "pinhal").with_parent(Some(&b.id)))
        .await
        .expect("cria C filha de B");
    assert_eq!(c.parent_id.as_deref(), Some(b.id.as_str()));

    cleanup(&pool, &project.id).await;
    pool.close().await;
}

/// F8 — `update_partial` altera SOMENTE os campos presentes no patch
/// (prioridade/troca total zera campos; aqui não).
#[tokio::test]
async fn update_partial_preserva_campos_ausentes() {
    if !pg_enabled() {
        eprintln!("skipped: defina TA_TEST_PG=1 e rode scripts/test-pg.sh");
        return;
    }
    let pool = test_pool().await;
    let project = new_project(&pool, "fase2").await;
    let class = PgClassRepo::new(pool.clone())
        .create(&Class::new(&project.id, "agua"))
        .await
        .expect("classe");
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(&project.id, "rs"))
        .await
        .expect("ruleset");
    let rules = PgRuleRepo::new(pool.clone());
    rules
        .create(&Rule::new(&ruleset.id, &class.id, "agua", "ndvi < 0.1").with_priority(3))
        .await
        .expect("regra");

    // Só --active: prioridade, predicado e classe preservados.
    let updated = rules
        .update_partial(
            &ruleset.id,
            "agua",
            &RulePatch {
                is_active: Some(false),
                ..RulePatch::default()
            },
        )
        .await
        .expect("update parcial")
        .expect("regra existe");
    assert!(!updated.is_active);
    assert_eq!(updated.priority, 3);
    assert_eq!(updated.predicate, "ndvi < 0.1");
    assert_eq!(updated.class_id, class.id);

    // Só --priority: is_active e o resto preservados.
    let updated = rules
        .update_partial(
            &ruleset.id,
            "agua",
            &RulePatch {
                priority: Some(9),
                ..RulePatch::default()
            },
        )
        .await
        .expect("update parcial")
        .expect("regra existe");
    assert_eq!(updated.priority, 9);
    assert!(!updated.is_active, "ausente no patch → preserva false");

    // Regra inexistente → None; classe de outro projeto → erro tipado.
    assert!(
        rules
            .update_partial(
                &ruleset.id,
                "fantasma",
                &RulePatch {
                    priority: Some(1),
                    ..RulePatch::default()
                }
            )
            .await
            .expect("update de ausente")
            .is_none()
    );
    let other = new_project(&pool, "fase2").await;
    let foreign = PgClassRepo::new(pool.clone())
        .create(&Class::new(&other.id, "agua"))
        .await
        .expect("classe de outro projeto");
    let err = rules
        .update_partial(
            &ruleset.id,
            "agua",
            &RulePatch {
                class_id: Some(foreign.id),
                ..RulePatch::default()
            },
        )
        .await
        .expect_err("classe cruzada deve falhar");
    assert!(matches!(err, DbError::ClassProjectMismatch { .. }), "{err}");

    cleanup(&pool, &project.id).await;
    cleanup(&pool, &other.id).await;
    pool.close().await;
}
