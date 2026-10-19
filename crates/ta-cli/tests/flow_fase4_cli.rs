//! **Exchange round-trip pela CLI** (Fase 4) — **gated** por `TA_TEST_PG=1`,
//! no harness de `scripts/test-pg.sh`.
//!
//! Fluxo real com o binário: projeto com fase, AOI, grade (2 células),
//! classes, ruleset+regras, classificação (`task run`), medidas com cena →
//! `exchange export` → `exchange import --into projeto_b` → conferência de
//! CONTAGENS por tabela, IGUALDADE GEOMÉTRICA (`a.geom = b.geom` por
//! (nome da grade, cell_id)), MESMA DECISÃO de `rule eval` para amostra das
//! medidas, e as RECUSAS do import (projeto existente, JSON corrompido,
//! versão errada, classe inexistente, predicado inválido). Também exercita
//! `scene add/link/list/unlink/remove` e `measure set --scene`.

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

fn run_ok(args: &[&str], ctx: &str) -> String {
    let out = run_cli(args);
    assert!(
        out.status.success(),
        "{ctx} falhou:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
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

/// Contagem no projeto (por `projects.name`) — tabela selecionada em código
/// (SQL literal; o nome nunca vem de input externo).
async fn count_for_project(pool: &PgPool, table: CountTable, project: &str) -> i64 {
    let sql = match table {
        CountTable::Phases => {
            "SELECT count(*) FROM phases t JOIN projects p ON t.project_id = p.id \
             WHERE p.name = $1"
        }
        CountTable::AoiLayers => {
            "SELECT count(*) FROM aoi_layers t JOIN projects p ON t.project_id = p.id \
             WHERE p.name = $1"
        }
        CountTable::CellLayers => {
            "SELECT count(*) FROM cell_layers t JOIN projects p ON t.project_id = p.id \
             WHERE p.name = $1"
        }
        CountTable::ClassGroups => {
            "SELECT count(*) FROM class_groups t JOIN projects p ON t.project_id = p.id \
             WHERE p.name = $1"
        }
        CountTable::Classes => {
            "SELECT count(*) FROM classes t JOIN projects p ON t.project_id = p.id \
             WHERE p.name = $1"
        }
        CountTable::Rulesets => {
            "SELECT count(*) FROM rulesets t JOIN projects p ON t.project_id = p.id \
             WHERE p.name = $1"
        }
        CountTable::DraftLayers => {
            "SELECT count(*) FROM draft_layers t JOIN projects p ON t.project_id = p.id \
             WHERE p.name = $1"
        }
    };
    sqlx::query_scalar(sql)
        .bind(project)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("count de {table:?} em {project}: {e}"))
}

/// Tabelas com `project_id` direto — para [`count_for_project`].
#[derive(Debug, Clone, Copy)]
enum CountTable {
    Phases,
    AoiLayers,
    CellLayers,
    ClassGroups,
    Classes,
    Rulesets,
    DraftLayers,
}

/// Semeia o projeto completo do round-trip (fixtures por `ta-db`, comandos do
/// fluxo pelo BINÁRIO). Devolve (nome do projeto, id da grade, id do ruleset).
async fn seed_full_project(pool: &PgPool, prefix: &str) -> (String, String, String) {
    use geo_types::{Coord, LineString, Polygon};
    use ta_core::aoi::AoiLayer;
    use ta_core::cell::{Cell, CellLayer, GridSpec};
    use ta_core::classification::{Class, ClassGroup};
    use ta_core::feature::Feature;
    use ta_core::layer::Layer;
    use ta_core::project::Project;
    use ta_core::rule::RuleSet;
    use ta_db::repo::{
        FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo,
    };
    use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
    use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
    use ta_db::repo_class::{ClassGroupRepo, ClassRepo, PgClassGroupRepo, PgClassRepo};
    use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};
    use ta_db::spatial::TARGET_SRID;

    let name = unique_name(prefix);
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: name.clone(),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("projeto do round-trip");

    // Fase (via binário — comando da Fase 1, não alvo desta fase).
    run_ok(
        &[
            "phase",
            "create",
            "--project",
            &name,
            "--name",
            "monitoramento",
            "--cell-blocker",
            "--dsn",
            &dsn(),
        ],
        "phase create",
    );

    // Camada AOI + 1 feição poligonal.
    let layer = PgLayerRepo::new(pool.clone())
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: format!("{prefix}_aoi_layer"),
            geom_type: ta_core::layer::GeomType::Polygon,
            srid: TARGET_SRID,
            source_uri: "teste-fase4".to_owned(),
        })
        .await
        .expect("camada AOI");
    let aoi_geom = Polygon::new(
        LineString::from(vec![
            Coord { x: -52.0, y: -12.0 },
            Coord {
                x: -51.99,
                y: -12.0,
            },
            Coord {
                x: -51.99,
                y: -11.99,
            },
            Coord {
                x: -52.0,
                y: -11.99,
            },
            Coord { x: -52.0, y: -12.0 },
        ]),
        vec![],
    );
    PgFeatureRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            vec![Feature::new(aoi_geom.into(), serde_json::json!({"cod": 1}))],
        )
        .await
        .expect("feature da AOI");
    PgAoiLayerRepo::new(pool.clone())
        .create(&AoiLayer::new(
            &project.id,
            &layer.id,
            Some("cod".to_owned()),
        ))
        .await
        .expect("vínculo de AOI");

    // Grade 2×1 com 1 bloqueio.
    let cell_layer = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project.id,
            /* aoi_layer_id */
            &{
                let id: String = sqlx::query_scalar(
                    "SELECT id::text FROM aoi_layers WHERE project_id = $1::uuid",
                )
                .bind(&project.id)
                .fetch_one(pool)
                .await
                .expect("aoi id");
                id
            },
            "grade_rt",
            &GridSpec::new(0.005, -52.0, -12.0, 4674),
        ))
        .await
        .expect("camada de células");
    let cell = |col: i32| Cell {
        id: 0,
        cell_layer_id: cell_layer.id.clone(),
        cell_id: format!("{col}_0"),
        geom: Polygon::new(
            LineString::from(vec![
                Coord {
                    x: -52.0 + 0.005 * f64::from(col),
                    y: -12.0,
                },
                Coord {
                    x: -52.0 + 0.005 * f64::from(col + 1),
                    y: -12.0,
                },
                Coord {
                    x: -52.0 + 0.005 * f64::from(col + 1),
                    y: -11.995,
                },
                Coord {
                    x: -52.0 + 0.005 * f64::from(col),
                    y: -11.995,
                },
                Coord {
                    x: -52.0 + 0.005 * f64::from(col),
                    y: -12.0,
                },
            ]),
            vec![],
        ),
        row_index: 0,
        col_index: col,
    };
    let inserted = PgCellRepo::new(pool.clone())
        .bulk_insert(&cell_layer.id, &[cell(0), cell(1)], 4674)
        .await
        .expect("células");
    assert_eq!(inserted, 2);

    // Classes: grupo + mãe + filha.
    let group = PgClassGroupRepo::new(pool.clone())
        .create(&ClassGroup::new(&project.id, "cobertura"))
        .await
        .expect("grupo");
    let floresta = PgClassRepo::new(pool.clone())
        .create(&{
            let mut c = Class::new(&project.id, "floresta");
            c.class_group_id = Some(group.id.clone());
            c.color = Some("#00AA00".to_owned());
            c
        })
        .await
        .expect("classe floresta");
    PgClassRepo::new(pool.clone())
        .create(&{
            let mut c = Class::new(&project.id, "degradada");
            c.parent_id = Some(floresta.id.clone());
            c.sort_order = 2;
            c.is_active = false;
            c
        })
        .await
        .expect("classe degradada");

    // Ruleset v1 + 2 regras + ativação.
    let ruleset = PgRuleSetRepo::new(pool.clone())
        .create(&RuleSet::new(&project.id, "regras_rt"))
        .await
        .expect("ruleset");
    let degradada_id: String = sqlx::query_scalar(
        "SELECT id::text FROM classes WHERE project_id = $1::uuid AND name = 'degradada'",
    )
    .bind(&project.id)
    .fetch_one(pool)
    .await
    .expect("id degradada");
    PgRuleRepo::new(pool.clone())
        .create(&{
            let mut r = ta_core::rule::Rule::new(
                &ruleset.id,
                &floresta.id,
                "floresta ndvi alto",
                "ndvi >= 0.6",
            );
            r.priority = 1;
            r
        })
        .await
        .expect("regra floresta");
    PgRuleRepo::new(pool.clone())
        .create(&ta_core::rule::Rule::new(
            &ruleset.id,
            &degradada_id,
            "degradada ndvi baixo",
            "ndvi < 0.6",
        ))
        .await
        .expect("regra degradada");
    run_ok(
        &[
            "ruleset",
            "activate",
            "--project",
            &name,
            "--name",
            "regras_rt",
            "--dsn",
            &dsn(),
        ],
        "ruleset activate",
    );

    // Medidas com cena (Fase 4): cena + proveniência via `measure set --scene`.
    run_ok(
        &[
            "scene",
            "add",
            "--name",
            &format!("cena_rt_{prefix}"),
            "--acquisition",
            "2026-01-31",
            "--footprint",
            "POLYGON((-52.01 -12.01,-52.01 -11.98,-51.98 -11.98,-51.98 -12.01,-52.01 -12.01))",
            "--metadata",
            "{\"satellite\":\"S2\"}",
            "--dsn",
            &dsn(),
        ],
        "scene add",
    );
    let scene_name = format!("cena_rt_{prefix}");
    run_ok(
        &[
            "scene",
            "link",
            "--project",
            &name,
            "--scene",
            &scene_name,
            "--dsn",
            &dsn(),
        ],
        "scene link",
    );
    for (cell_id, ndvi) in [("0_0", "0.72"), ("1_0", "0.35")] {
        run_ok(
            &[
                "measure",
                "set",
                "--cell-layer",
                &cell_layer.id,
                "--cell-id",
                cell_id,
                "--values",
                &format!("ndvi={ndvi}"),
                "--scene",
                &scene_name,
                "--dsn",
                &dsn(),
            ],
            "measure set --scene",
        );
    }

    // Classificação real pelo binário (task create + run → rascunho + decisões).
    let out = run_ok(
        &[
            "task",
            "create",
            "classify",
            "--project",
            &name,
            "--cell-layer",
            &cell_layer.id,
            "--dsn",
            &dsn(),
        ],
        "task create",
    );
    let task_id = out
        .lines()
        .find_map(|l| l.strip_prefix("task criada: "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .filter(|id| id.len() == 36)
        .expect("id da task na saída");
    run_ok(&["task", "run", &task_id, "--dsn", &dsn()], "task run");

    (name, cell_layer.id.clone(), ruleset.id.clone())
}

/// Teste central: round-trip export → import com contagens, geometria e
/// decisão de regra iguais.
#[tokio::test]
async fn exchange_round_trip_preserva_contagens_geometria_e_decisao() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = migrate_and_pool().await;
    let prefix = unique_name("rt");
    let (proj_a, _grade_a, _ruleset_a) = seed_full_project(&pool, &prefix).await;
    let proj_b = format!("{prefix}_b");

    let dir = std::env::temp_dir().join(format!("exchange_rt_{prefix}.json"));
    let export_out = run_ok(
        &[
            "exchange",
            "export",
            "--project",
            &proj_a,
            "--out",
            dir.to_str().expect("caminho do exchange"),
            "--dsn",
            &dsn(),
        ],
        "exchange export",
    );
    assert!(
        export_out.contains("celulas=2") && export_out.contains("regras=2"),
        "contagens por seção na saída do export: {export_out}"
    );

    run_ok(
        &[
            "exchange",
            "import",
            "--in",
            dir.to_str().expect("caminho do exchange"),
            "--into",
            &proj_b,
            "--dsn",
            &dsn(),
        ],
        "exchange import",
    );

    // -- CONTAGENS por tabela (A vs B) --------------------------------------
    for table in [
        CountTable::Phases,
        CountTable::AoiLayers,
        CountTable::CellLayers,
        CountTable::ClassGroups,
        CountTable::Classes,
        CountTable::Rulesets,
        CountTable::DraftLayers,
    ] {
        let (a, b) = (
            count_for_project(&pool, table, &proj_a).await,
            count_for_project(&pool, table, &proj_b).await,
        );
        assert_eq!(a, b, "contagem difere (A={a}, B={b}): {table:?}");
    }
    let (rules_a, rules_b): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM rules v JOIN rulesets rs ON v.ruleset_id = rs.id \
             JOIN projects p ON rs.project_id = p.id WHERE p.name = $1), \
           (SELECT count(*) FROM rules v JOIN rulesets rs ON v.ruleset_id = rs.id \
             JOIN projects p ON rs.project_id = p.id WHERE p.name = $2)",
    )
    .bind(&proj_a)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("count de rules");
    assert_eq!(rules_a, rules_b, "contagem de rules difere");
    let (dc_a, dc_b): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM draft_cells v JOIN draft_layers dl ON v.draft_layer_id = dl.id \
             JOIN projects p ON dl.project_id = p.id WHERE p.name = $1), \
           (SELECT count(*) FROM draft_cells v JOIN draft_layers dl ON v.draft_layer_id = dl.id \
             JOIN projects p ON dl.project_id = p.id WHERE p.name = $2)",
    )
    .bind(&proj_a)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("count de draft_cells");
    assert_eq!(dc_a, dc_b, "contagem de draft_cells difere");
    // Tabelas sem project_id direto (via projeto da grade/camada).
    let (blk_a, blk_b): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM blocked_cells v \
              JOIN cell_layers cl ON v.cell_layer_id = cl.id \
              JOIN projects p ON cl.project_id = p.id WHERE p.name = $1), \
           (SELECT count(*) FROM blocked_cells v \
              JOIN cell_layers cl ON v.cell_layer_id = cl.id \
              JOIN projects p ON cl.project_id = p.id WHERE p.name = $2)",
    )
    .bind(&proj_a)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("count de blocked_cells");
    assert_eq!(blk_a, blk_b, "contagem de blocked_cells difere");
    let (med_a, med_b): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM cell_measures v \
              JOIN cell_layers cl ON v.cell_layer_id = cl.id \
              JOIN projects p ON cl.project_id = p.id WHERE p.name = $1), \
           (SELECT count(*) FROM cell_measures v \
              JOIN cell_layers cl ON v.cell_layer_id = cl.id \
              JOIN projects p ON cl.project_id = p.id WHERE p.name = $2)",
    )
    .bind(&proj_a)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("count de cell_measures");
    assert_eq!(med_a, med_b, "contagem de cell_measures difere");
    let (fea_a, fea_b): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM features v JOIN layers l ON v.layer_id = l.id \
              JOIN projects p ON l.project_id = p.id WHERE p.name = $1), \
           (SELECT count(*) FROM features v JOIN layers l ON v.layer_id = l.id \
              JOIN projects p ON l.project_id = p.id WHERE p.name = $2)",
    )
    .bind(&proj_a)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("count de features");
    assert_eq!(fea_a, fea_b, "contagem de features difere");

    // -- IGUALDADE GEOMÉTRICA por (nome da grade, cell_id) --------------------
    let (iguais, diferentes): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE a.geom = b.geom), \
                count(*) FILTER (WHERE NOT (a.geom = b.geom)) \
         FROM cells a \
         JOIN cell_layers la ON la.id = a.cell_layer_id \
         JOIN cell_layers lb ON lb.project_id = (SELECT id FROM projects WHERE name = $2) \
             AND lb.name = la.name \
         JOIN cells b ON b.cell_layer_id = lb.id AND b.cell_id = a.cell_id \
         WHERE la.project_id = (SELECT id FROM projects WHERE name = $1)",
    )
    .bind(&proj_a)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("comparação geométrica");
    assert!(iguais >= 2, "células comparadas: {iguais}");
    assert_eq!(diferentes, 0, "células com geometria diferente");

    // Cenas: política de REAPROVEITAMENTO (cena é global, nome é único) —
    // o import vincula o projeto B À MESMA cena, com o mesmo conteúdo.
    let cena = format!("cena_rt_{prefix}");
    let (vinculos, conteudo_ok): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE \
                s.acquisition_on = '2026-01-31 00:00:00+00' AND \
                ST_Equals(s.footprint, ST_GeomFromText(\
                    'MULTIPOLYGON(((-52.01 -12.01,-52.01 -11.98,-51.98 -11.98,-51.98 -12.01,-52.01 -12.01)))', \
                    4674))) \
         FROM project_scenes ps \
         JOIN projects p ON p.id = ps.project_id AND p.name = $2 \
         JOIN scenes s ON s.id = ps.scene_id \
         WHERE s.name = $1",
    )
    .bind(&cena)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("vínculo de cena do projeto B");
    assert_eq!(vinculos, 1, "projeto B vinculado à cena");
    assert_eq!(conteudo_ok, 1, "cena com aquisição e footprint do exchange");

    // Decisões de rascunho iguais (classe POR NOME) para as duas células.
    let (decisoes_iguais, decisoes_dif): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE ca.name = cb.name), \
                count(*) FILTER (WHERE ca.name <> cb.name) \
         FROM draft_cells da \
         JOIN draft_layers dla ON dla.id = da.draft_layer_id \
         JOIN projects pa ON dla.project_id = pa.id AND pa.name = $1 \
         JOIN classes ca ON ca.id = da.class_id \
         JOIN draft_layers dlb ON dlb.project_id = (SELECT id FROM projects WHERE name = $2) \
             AND dlb.name = dla.name \
         JOIN draft_cells db ON db.draft_layer_id = dlb.id AND db.cell_id = da.cell_id \
         JOIN classes cb ON cb.id = db.class_id",
    )
    .bind(&proj_a)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("comparação de decisões");
    assert_eq!(decisoes_iguais, 2, "decisões iguais por nome de classe");
    assert_eq!(decisoes_dif, 0, "decisões divergentes no round-trip");

    // Medidas: mesmo valor por (grade, célula, medida).
    let (medidas_iguais, medidas_dif): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE ma.value = mb.value), \
                count(*) FILTER (WHERE ma.value <> mb.value) \
         FROM cell_measures ma \
         JOIN cell_layers la ON la.id = ma.cell_layer_id \
         JOIN projects pa ON la.project_id = pa.id AND pa.name = $1 \
         JOIN cell_layers lb ON lb.project_id = (SELECT id FROM projects WHERE name = $2) \
             AND lb.name = la.name \
         JOIN cell_measures mb ON mb.cell_layer_id = lb.id \
             AND mb.cell_id = ma.cell_id AND mb.measure = ma.measure",
    )
    .bind(&proj_a)
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("comparação de medidas");
    assert_eq!(medidas_iguais, 2, "medidas iguais no round-trip");
    assert_eq!(medidas_dif, 0, "medidas divergentes");

    // -- MESMA DECISÃO de `rule eval` para amostra das medidas -----------------
    let ruleset_b_id: String = sqlx::query_scalar(
        "SELECT rs.id::text FROM rulesets rs \
         JOIN projects p ON rs.project_id = p.id \
         WHERE p.name = $1 AND rs.name = 'regras_rt'",
    )
    .bind(&proj_b)
    .fetch_one(&pool)
    .await
    .expect("ruleset do projeto B");
    let eval_a = run_ok(
        &[
            "rule",
            "eval",
            "--ruleset",
            &_ruleset_a,
            "--obs",
            "ndvi=0.72",
            "--dsn",
            &dsn(),
        ],
        "rule eval A",
    );
    let eval_b = run_ok(
        &[
            "rule",
            "eval",
            "--ruleset",
            &ruleset_b_id,
            "--obs",
            "ndvi=0.72",
            "--dsn",
            &dsn(),
        ],
        "rule eval B",
    );
    // Compara só o NOME da classe decidida (os UUIDs diferem entre projetos).
    let decisao = |out: &str| {
        let linha = out
            .lines()
            .find(|l| l.starts_with("decisão: classe="))
            .unwrap_or_else(|| panic!("sem decisão no eval: {out}"));
        linha
            .split("classe=")
            .nth(1)
            .and_then(|resto| resto.split_whitespace().next())
            .unwrap_or_else(|| panic!("decisão sem classe: {linha}"))
            .to_owned()
    };
    assert_eq!(
        decisao(&eval_a),
        decisao(&eval_b),
        "mesma medida → mesma decisão nos dois projetos"
    );

    // -- RECUSAS do import ------------------------------------------------------
    // 1) Projeto existente.
    assert_cli_fails(
        &[
            "exchange",
            "import",
            "--in",
            dir.to_str().expect("caminho"),
            "--into",
            &proj_b,
            "--dsn",
            &dsn(),
        ],
        "já existe",
    );
    // 2) JSON corrompido.
    let ruim = std::env::temp_dir().join(format!("exchange_ruim_{prefix}.json"));
    std::fs::write(&ruim, "{ format: quebrado").expect("escreve JSON ruim");
    assert_cli_fails(
        &[
            "exchange",
            "import",
            "--in",
            ruim.to_str().expect("caminho"),
            "--into",
            &unique_name("nunca"),
            "--dsn",
            &dsn(),
        ],
        "JSON do exchange inválido",
    );
    // 3) Versão errada.
    let texto = std::fs::read_to_string(&dir).expect("lê exchange");
    let v2 = texto.replace("\"version\": 1", "\"version\": 99");
    std::fs::write(&ruim, v2).expect("escreve versão errada");
    assert_cli_fails(
        &[
            "exchange",
            "import",
            "--in",
            ruim.to_str().expect("caminho"),
            "--into",
            &unique_name("nunca"),
            "--dsn",
            &dsn(),
        ],
        "versão de exchange não suportada",
    );
    // 4) Classe inexistente (referenciada por uma regra).
    let sem_classe = texto.replace(
        "\"class_name\": \"floresta\"",
        "\"class_name\": \"inexistente\"",
    );
    std::fs::write(&ruim, sem_classe).expect("escreve classe inexistente");
    assert_cli_fails(
        &[
            "exchange",
            "import",
            "--in",
            ruim.to_str().expect("caminho"),
            "--into",
            &unique_name("nunca"),
            "--dsn",
            &dsn(),
        ],
        "nenhuma classe com esse nome",
    );
    // 5) Predicado inválido — recusado ANTES de gravar a regra.
    let predicado_ruim = texto.replace("ndvi >= 0.6", "ndvi ~~~ 0.6");
    std::fs::write(&ruim, predicado_ruim).expect("escreve predicado inválido");
    assert_cli_fails(
        &[
            "exchange",
            "import",
            "--in",
            ruim.to_str().expect("caminho"),
            "--into",
            &unique_name("nunca"),
            "--dsn",
            &dsn(),
        ],
        "predicado inválido",
    );
    // 6) Projeto inexistente no export.
    assert_cli_fails(
        &[
            "exchange",
            "export",
            "--project",
            &unique_name("fantasma"),
            "--out",
            "/tmp/opencode/nunca.json",
            "--dsn",
            &dsn(),
        ],
        "não existe",
    );

    let _ = (_grade_a, _ruleset_a);
    let _ = count_for_project(&pool, CountTable::Phases, &proj_b).await;
    pool.close().await;
}

/// Import ATÔMICO: falha injetada no MEIO do documento (classe inexistente
/// referenciada por regra — a seção `rulesets` falha DEPOIS de projeto/cenas/
/// fases/aoi/grades/grupos/classes já gravados na transação) não deixa NADA
/// no banco; o RETRY do mesmo `--into` funciona imediatamente.
#[tokio::test]
async fn import_falho_nao_deixa_projeto_parcial_e_retry_funciona() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = migrate_and_pool().await;
    let prefix = unique_name("atom");
    let (proj_a, _grade_a, _ruleset_a) = seed_full_project(&pool, &prefix).await;
    let proj_c = format!("{prefix}_c");

    let dir = std::env::temp_dir().join(format!("exchange_atom_{prefix}.json"));
    run_ok(
        &[
            "exchange",
            "export",
            "--project",
            &proj_a,
            "--out",
            dir.to_str().expect("caminho do exchange"),
            "--dsn",
            &dsn(),
        ],
        "exchange export",
    );

    let texto = std::fs::read_to_string(&dir).expect("lê exchange");
    let quebrado = texto.replace(
        "\"class_name\": \"floresta\"",
        "\"class_name\": \"inexistente\"",
    );
    let ruim = std::env::temp_dir().join(format!("exchange_atom_ruim_{prefix}.json"));
    std::fs::write(&ruim, quebrado).expect("escreve doc quebrado");

    // Falha com o erro acionável E o caminho de saída exato (defesa em
    // profundidade do import atômico).
    let out = run_cli(&[
        "exchange",
        "import",
        "--in",
        ruim.to_str().expect("caminho"),
        "--into",
        &proj_c,
        "--dsn",
        &dsn(),
    ]);
    assert!(!out.status.success(), "import quebrado não pode passar");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.contains("nenhuma classe com esse nome"),
        "erro sem a causa: {combined}"
    );
    assert!(
        combined.contains(&format!("project delete --project {proj_c} --yes")),
        "erro deve citar o caminho de saída: {combined}"
    );

    // NADA parcial: o projeto não existe no banco.
    let projeto: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE name = $1")
        .bind(&proj_c)
        .fetch_one(&pool)
        .await
        .expect("count projeto");
    assert_eq!(projeto, 0, "import falho NÃO pode deixar projeto parcial");

    // RETRY do MESMO --into agora FUNCIONA (nada ficou bloqueado).
    let out = run_ok(
        &[
            "exchange",
            "import",
            "--in",
            dir.to_str().expect("caminho"),
            "--into",
            &proj_c,
            "--dsn",
            &dsn(),
        ],
        "retry do import",
    );
    assert!(out.contains("exchange importado"), "{out}");
    let projeto: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE name = $1")
        .bind(&proj_c)
        .fetch_one(&pool)
        .await
        .expect("count projeto pós-retry");
    assert_eq!(projeto, 1, "retry cria o projeto exatamente uma vez");
    pool.close().await;
}

/// Cena reaproveitada com DIVERGÊNCIA: o import NÃO falha e NÃO sobrescreve —
/// emite AVISO no relatório e a cena global permanece com o conteúdo original.
#[tokio::test]
async fn import_cena_divergente_emite_aviso_e_reaproveita() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = migrate_and_pool().await;
    let prefix = unique_name("cdiv");
    let (proj_a, _grade_a, _ruleset_a) = seed_full_project(&pool, &prefix).await;
    let proj_d = format!("{prefix}_d");
    let cena = format!("cena_rt_{prefix}");

    let dir = std::env::temp_dir().join(format!("exchange_cdiv_{prefix}.json"));
    run_ok(
        &[
            "exchange",
            "export",
            "--project",
            &proj_a,
            "--out",
            dir.to_str().expect("caminho do exchange"),
            "--dsn",
            &dsn(),
        ],
        "exchange export",
    );

    // Footprint DIFERENTE no documento (a cena já existe no banco com outro).
    let texto = std::fs::read_to_string(&dir).expect("lê exchange");
    let divergente = texto.replace(
        "MULTIPOLYGON(((-52.01 -12.01,-52.01 -11.98,-51.98 -11.98,-51.98 -12.01,-52.01 -12.01)))",
        "MULTIPOLYGON(((-53 -13,-53 -12.9,-52.9 -12.9,-52.9 -13,-53 -13)))",
    );
    assert_ne!(texto, divergente, "footprint não encontrado no documento");
    std::fs::write(&dir, divergente).expect("escreve doc com footprint divergente");

    let out = run_ok(
        &[
            "exchange",
            "import",
            "--in",
            dir.to_str().expect("caminho"),
            "--into",
            &proj_d,
            "--dsn",
            &dsn(),
        ],
        "import com cena divergente",
    );
    assert!(
        out.contains("aviso:") && out.contains(&cena),
        "aviso de divergência ausente no relatório: {out}"
    );
    assert!(
        out.contains("já existia com footprint diferentes — reaproveitada sem alteração"),
        "texto do aviso errado: {out}"
    );

    // A cena NÃO foi sobrescrita: conteúdo original preservado (subqueries
    // escalares — a cena tem vínculo com A e D, join multiplicaria linhas).
    let (conteudo_ok, vinculo_novo): (i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM scenes s WHERE s.name = $1 AND \
                s.acquisition_on = '2026-01-31 00:00:00+00' AND \
                ST_Equals(s.footprint, ST_GeomFromText(\
                    'MULTIPOLYGON(((-52.01 -12.01,-52.01 -11.98,-51.98 -11.98,-51.98 -12.01,-52.01 -12.01)))', \
                    4674))), \
           (SELECT count(*) FROM project_scenes ps \
              JOIN projects p ON p.id = ps.project_id \
              WHERE p.name = $2 AND ps.scene_id = (SELECT id FROM scenes WHERE name = $1))",
    )
    .bind(&cena)
    .bind(&proj_d)
    .fetch_one(&pool)
    .await
    .expect("cena preservada");
    assert_eq!(conteudo_ok, 1, "cena divergente foi sobrescrita?");
    assert_eq!(vinculo_novo, 1, "projeto D precisa ficar vinculado à cena");
    pool.close().await;
}

/// `project delete`: sem `--yes` recusa mostrando o resumo; com `--yes` faz a
/// deleção ORDENADA (FK RESTRICT respeitada SEM desativar trigger) com
/// relatório de contagens e deixa ZERO linhas em TODAS as tabelas do projeto;
/// a cena GLOBAL sobrevive (só o vínculo sai).
#[tokio::test]
async fn project_delete_ordena_e_zera_banco() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = migrate_and_pool().await;
    let prefix = unique_name("del");
    let (proj, grade, _ruleset) = seed_full_project(&pool, &prefix).await;
    // Bloqueio real (o seed não bloqueia; o projeto rico precisa de 1).
    run_ok(
        &[
            "cell",
            "block",
            "--cell-layer",
            &grade,
            "--cell-id",
            "1_0",
            "--reason",
            "nuvem",
            "--dsn",
            &dsn(),
        ],
        "cell block",
    );

    // ids capturados ANTES da deleção — a prova de zero é POR ID (join com
    // projects zeraria trivialmente depois do delete do projeto).
    let (pid, fase, camada_aoi, aoi, rascunho, task, grupo, cena): (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT \
           p.id::text, \
           (SELECT id::text FROM phases WHERE project_id = p.id LIMIT 1), \
           (SELECT id::text FROM layers WHERE project_id = p.id LIMIT 1), \
           (SELECT id::text FROM aoi_layers WHERE project_id = p.id LIMIT 1), \
           (SELECT id::text FROM draft_layers WHERE project_id = p.id LIMIT 1), \
           (SELECT id::text FROM tasks WHERE project_id = p.id LIMIT 1), \
           (SELECT id::text FROM class_groups WHERE project_id = p.id LIMIT 1), \
           (SELECT s.id::text FROM scenes s JOIN project_scenes ps ON ps.scene_id = s.id \
              WHERE ps.project_id = p.id LIMIT 1) \
         FROM projects p WHERE p.name = $1",
    )
    .bind(&proj)
    .fetch_one(&pool)
    .await
    .expect("ids do projeto rico");
    assert!(cena.len() == 36, "projeto rico precisa de cena vinculada");

    // -- sem --yes: recusa com o resumo do que seria apagado ----------------
    let out = run_cli(&["project", "delete", "--project", &proj, "--dsn", &dsn()]);
    assert!(!out.status.success(), "delete sem --yes não pode passar");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.contains("recusado sem --yes"), "{combined}");
    assert!(
        combined.contains("total=") || combined.contains("linha(s)"),
        "{combined}"
    );
    // Nada foi apagado pela pré-checagem.
    let ainda: i64 = sqlx::query_scalar("SELECT count(*) FROM projects WHERE id = $1::uuid")
        .bind(&pid)
        .fetch_one(&pool)
        .await
        .expect("projeto após preview");
    assert_eq!(ainda, 1, "preview não pode apagar nada");

    // -- com --yes: apaga TUDO com relatório --------------------------------
    let out = run_ok(
        &[
            "project",
            "delete",
            "--project",
            &proj,
            "--yes",
            "--dsn",
            &dsn(),
        ],
        "project delete --yes",
    );
    assert!(out.contains("projeto apagado"), "{out}");
    assert!(out.contains("draft_cells=2"), "relatório: {out}");
    assert!(out.contains("rules=2"), "relatório: {out}");
    assert!(out.contains("rulesets=1"), "relatório: {out}");
    assert!(out.contains("cells=2"), "relatório: {out}");
    assert!(out.contains("cell_measures=2"), "relatório: {out}");
    assert!(out.contains("blocked_cells=1"), "relatório: {out}");
    assert!(out.contains("classes=2"), "relatório: {out}");
    assert!(out.contains("phases=1"), "relatório: {out}");

    // 0 linhas em TODAS as tabelas do projeto (SQL literal fixo, bind-only;
    // contagem por id capturado antes da deleção).
    let zeros: [(&str, &str, &str); 18] = [
        (
            "draft_cells",
            "SELECT count(*) FROM draft_cells WHERE draft_layer_id = $1::uuid",
            &rascunho,
        ),
        (
            "rules",
            "SELECT count(*) FROM rules r JOIN rulesets rs ON r.ruleset_id = rs.id WHERE rs.project_id = $1::uuid",
            &pid,
        ),
        (
            "rulesets",
            "SELECT count(*) FROM rulesets WHERE project_id = $1::uuid",
            &pid,
        ),
        (
            "cell_log",
            "SELECT count(*) FROM cell_log WHERE task_id = $1::uuid",
            &task,
        ),
        (
            "processes",
            "SELECT count(*) FROM processes WHERE task_id = $1::uuid",
            &task,
        ),
        (
            "tasks",
            "SELECT count(*) FROM tasks WHERE id = $1::uuid",
            &task,
        ),
        (
            "cell_measures",
            "SELECT count(*) FROM cell_measures WHERE cell_layer_id = $1::uuid",
            &grade,
        ),
        (
            "blocked_cells",
            "SELECT count(*) FROM blocked_cells WHERE cell_layer_id = $1::uuid",
            &grade,
        ),
        (
            "cells",
            "SELECT count(*) FROM cells WHERE cell_layer_id = $1::uuid",
            &grade,
        ),
        (
            "cell_layers",
            "SELECT count(*) FROM cell_layers WHERE id = $1::uuid",
            &grade,
        ),
        (
            "draft_layers",
            "SELECT count(*) FROM draft_layers WHERE id = $1::uuid",
            &rascunho,
        ),
        (
            "features",
            "SELECT count(*) FROM features WHERE layer_id = $1::uuid",
            &camada_aoi,
        ),
        (
            "layers",
            "SELECT count(*) FROM layers WHERE id = $1::uuid",
            &camada_aoi,
        ),
        (
            "aoi_layers",
            "SELECT count(*) FROM aoi_layers WHERE id = $1::uuid",
            &aoi,
        ),
        (
            "project_scenes",
            "SELECT count(*) FROM project_scenes WHERE project_id = $1::uuid",
            &pid,
        ),
        (
            "class_groups",
            "SELECT count(*) FROM class_groups WHERE id = $1::uuid",
            &grupo,
        ),
        (
            "phases",
            "SELECT count(*) FROM phases WHERE id = $1::uuid",
            &fase,
        ),
        (
            "projects",
            "SELECT count(*) FROM projects WHERE id = $1::uuid",
            &pid,
        ),
    ];
    for (tabela, sql, id) in zeros {
        let n: i64 = sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("count de {tabela} pós-delete: {e}"));
        assert_eq!(n, 0, "tabela {tabela} não zerou (ficaram {n} linha(s))");
    }
    // Classes: mãe (floresta) e filha (degradada) — 0 no projeto inteiro.
    let classes_restantes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM classes WHERE project_id = $1::uuid")
            .bind(&pid)
            .fetch_one(&pool)
            .await
            .expect("classes pós-delete");
    assert_eq!(classes_restantes, 0, "classes (mãe e filha) não zeraram");

    // A cena GLOBAL sobrevive: 1 linha em scenes, 0 vínculos.
    let (cena_viva, vinculos): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM scenes WHERE id = $1::uuid), \
                (SELECT count(*) FROM project_scenes WHERE scene_id = $1::uuid)",
    )
    .bind(&cena)
    .fetch_one(&pool)
    .await
    .expect("cena global pós-delete");
    assert_eq!(cena_viva, 1, "cena global deve sobreviver ao projeto");
    assert_eq!(vinculos, 0, "vínculo do projeto apagado deve sair");
    pool.close().await;
}

#[tokio::test]
async fn scene_lifecycle_pela_cli() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = migrate_and_pool().await;
    let project = unique_name("proj_scene_cli");
    let scene = unique_name("cena_cli");

    run_ok(
        &[
            "scene",
            "add",
            "--name",
            &scene,
            "--acquisition",
            "2026-02-10T13:45:00Z",
            "--dsn",
            &dsn(),
        ],
        "scene add",
    );
    // Nome duplicado → erro tipado acionável.
    assert_cli_fails(
        &["scene", "add", "--name", &scene, "--dsn", &dsn()],
        "já existe",
    );

    // Cena é global: aparece no `scene list` SEM projeto (ainda sem vínculo).
    let out = run_ok(&["scene", "list", "--dsn", &dsn()], "scene list global");
    assert!(out.contains(&scene), "cena listada globalmente: {out}");

    run_ok(
        &[
            "scene",
            "link",
            "--project",
            &project,
            "--scene",
            &scene,
            "--dsn",
            &dsn(),
        ],
        "scene link",
    );
    // Depois do vínculo, o filtro por projeto mostra a cena.
    let out = run_ok(
        &["scene", "list", "--project", &project, "--dsn", &dsn()],
        "scene list por projeto",
    );
    assert!(out.contains(&scene), "cena listada no projeto: {out}");
    let out = run_ok(
        &[
            "scene",
            "link",
            "--project",
            &project,
            "--scene",
            &scene,
            "--dsn",
            &dsn(),
        ],
        "scene link repetido",
    );
    assert!(out.contains("já estava vinculada"), "{out}");

    // Cena inexistente → erro acionável apontando `scene add`.
    assert_cli_fails(
        &[
            "scene",
            "link",
            "--project",
            &project,
            "--scene",
            "nunca_existe",
            "--dsn",
            &dsn(),
        ],
        "não existe",
    );

    run_ok(
        &[
            "scene",
            "unlink",
            "--project",
            &project,
            "--scene",
            &scene,
            "--dsn",
            &dsn(),
        ],
        "scene unlink",
    );

    // Remove sem --yes: recusa mostrando o efeito.
    assert_cli_fails(
        &["scene", "remove", "--name", &scene, "--dsn", &dsn()],
        "--yes",
    );
    run_ok(
        &[
            "scene",
            "remove",
            "--name",
            &scene,
            "--yes",
            "--dsn",
            &dsn(),
        ],
        "scene remove",
    );
    let out = run_ok(&["scene", "list", "--dsn", &dsn()], "scene list final");
    assert!(
        !out.contains(&scene),
        "cena removida não deve listar: {out}"
    );

    let _ = &pool;
    pool.close().await;
}

/// `measure set --scene` com cena inexistente → erro acionável.
#[tokio::test]
async fn measure_set_com_cena_inexistente_falha_acionavel() {
    if !pg_enabled() {
        eprintln!("skipped (TA_TEST_PG != 1)");
        return;
    }
    let pool = migrate_and_pool().await;
    // Fixture mínimo: projeto + camada de células + 1 célula (via ta-db).
    use ta_core::cell::{Cell, CellLayer, GridSpec};
    use ta_core::project::Project;
    use ta_db::repo::{PgProjectRepo, ProjectRepo};
    use ta_db::repo_cell::{CellLayerRepo, CellRepo, PgCellLayerRepo, PgCellRepo};
    use ta_db::spatial::TARGET_SRID;
    let project = PgProjectRepo::new(pool.clone())
        .create(&Project {
            id: String::new(),
            name: unique_name("proj_med_cli"),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("projeto");
    let aoi_layer_id: String = {
        use ta_core::feature::Feature;
        use ta_core::layer::{GeomType, Layer};
        use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo};
        use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
        let layer = PgLayerRepo::new(pool.clone())
            .add(&Layer {
                id: String::new(),
                project_id: project.id.clone(),
                name: unique_name("aoi_med_cli"),
                geom_type: GeomType::Polygon,
                srid: TARGET_SRID,
                source_uri: "teste-fase4-cli".to_owned(),
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
            .expect("feature da AOI");
        let aoi = PgAoiLayerRepo::new(pool.clone())
            .create(&ta_core::aoi::AoiLayer::new(&project.id, &layer.id, None))
            .await
            .expect("vínculo de AOI");
        aoi.id
    };
    let layer = PgCellLayerRepo::new(pool.clone())
        .create(&CellLayer::new(
            &project.id,
            &aoi_layer_id,
            "grade_med_cli",
            &GridSpec::new(0.005, -52.0, -12.0, 4674),
        ))
        .await
        .expect("grade");
    let cell_geom: geo_types::Polygon<f64> = geo_types::Polygon::new(
        geo_types::LineString::from(vec![
            (-52.0, -12.0),
            (-51.995, -12.0),
            (-51.995, -11.995),
            (-52.0, -11.995),
            (-52.0, -12.0),
        ]),
        vec![],
    );
    PgCellRepo::new(pool.clone())
        .bulk_insert(
            &layer.id,
            &[Cell {
                id: 0,
                cell_layer_id: layer.id.clone(),
                cell_id: "0_0".to_owned(),
                geom: cell_geom,
                row_index: 0,
                col_index: 0,
            }],
            4674,
        )
        .await
        .expect("célula");

    // Cena inexistente → erro acionável ANTES de gravar (aponta `scene add`).
    assert_cli_fails(
        &[
            "measure",
            "set",
            "--cell-layer",
            &layer.id,
            "--cell-id",
            "0_0",
            "--values",
            "ndvi=0.5",
            "--scene",
            "cena_que_nao_existe",
            "--dsn",
            &dsn(),
        ],
        "cena_que_nao_existe",
    );
    // Com a cena criada, grava e mostra a cena na saída.
    let cena = unique_name("cena_med_cli");
    run_ok(
        &["scene", "add", "--name", &cena, "--dsn", &dsn()],
        "scene add",
    );
    let out = run_ok(
        &[
            "measure",
            "set",
            "--cell-layer",
            &layer.id,
            "--cell-id",
            "0_0",
            "--values",
            "ndvi=0.5",
            "--scene",
            &cena,
            "--dsn",
            &dsn(),
        ],
        "measure set --scene",
    );
    assert!(out.contains(&cena), "saída menciona a cena: {out}");
    pool.close().await;
}
