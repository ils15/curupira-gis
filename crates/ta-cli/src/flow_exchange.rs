//! **Exchange do fluxo de monitoramento** (Fase 4): `exchange export` e
//! `exchange import`.
//!
//! Portabilidade por **NOMES estáveis** (projeto, fase, camada, `cell_id`,
//! classe, ruleset nome+versão, cena) — nunca por UUIDs. O modelo serde e a
//! validação do documento vivem em `ta-gis::exchange` (dono do formato); aqui
//! fica a orquestração no banco:
//!
//! - **export** coleta TUDO do projeto por id (mesmo sem dados — seções
//!   vazias são válidas) e escreve JSON pretty, com a contagem por seção.
//! - **import** cria um projeto NOVO (`--into`, recusa nome existente — sem
//!   merge nesta fase) e recria as seções na **ordem de dependência**:
//!   projeto → cenas(+vínculos) → fases → AOI(camada+features) →
//!   grades(células+bloqueios) → grupos → classes(ordem topológica por mãe)
//!   → rulesets(+regras, predicado validado ANTES de gravar) → rascunhos
//!   (+decisões) → medidas. Tudo numa ÚNICA transação (atômico: falha em
//!   qualquer seção não deixa NADA no banco) com relatório final e avisos de
//!   reaproveitamento de cena divergente.
//!
//! Geometria do documento é WKT **4674** (armazenamento canônico): células
//! entram direto (sem `ST_Transform`), então o `srid` da grade (unidades da
//! resolução) não interfere no round-trip.

use std::collections::BTreeMap;
use std::path::PathBuf;

use geo_types::{Geometry, MultiPolygon};
use sqlx::PgPool;
use ta_core::cell::Cell;
use ta_core::classification::{Class, ClassGroup, DraftLayer, DraftState};
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::phase::Phase;
use ta_core::rule::{Rule, RuleSet};
use ta_db::repo::{
    FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectFlowSettings,
    ProjectRepo,
};
use ta_db::repo_aoi::{AoiLayerRepo, PgAoiLayerRepo};
use ta_db::repo_cell::{
    BlockedCellRepo, CellLayerRepo, CellRepo, PgBlockedCellRepo, PgCellLayerRepo, PgCellRepo,
};
use ta_db::repo_class::{
    ClassGroupRepo, ClassRepo, DraftLayerRepo, PgClassGroupRepo, PgClassRepo, PgDraftLayerRepo,
};
use ta_db::repo_phase::{PgPhaseRepo, PhaseRepo};
use ta_db::repo_ruleset::{PgRuleRepo, PgRuleSetRepo, RuleRepo, RuleSetRepo};
use ta_db::repo_scene::{PgSceneRepo, SceneRepo};
use ta_db::repo_task::PgTaskRepo;
use ta_db::spatial::TARGET_SRID;
use ta_engine::parse_predicate;
use ta_gis::exchange::{
    ExchangeAoiLayer, ExchangeBlocked, ExchangeCell, ExchangeCellLayer, ExchangeClass,
    ExchangeClassGroup, ExchangeDoc, ExchangeDraftCell, ExchangeDraftLayer, ExchangeError,
    ExchangeFeature, ExchangeMeasure, ExchangePhase, ExchangeProject, ExchangeRule,
    ExchangeRuleset, ExchangeScene, geometry_to_wkt, parse_iso8601, parse_wkt,
    scene_footprint_from_wkt,
};

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

/// Resolve projeto EXISTENTE por id (UUID) ou nome — SEM auto-criar (o export
/// de um nome inexistente deve falhar, não criar projeto vazio).
async fn resolve_existing_project(
    pool: &PgPool,
    id_or_name: &str,
) -> Result<ta_core::project::Project, String> {
    let repo = PgProjectRepo::new(pool.clone());
    if id_or_name.len() == 36 && id_or_name.as_bytes()[8] == b'-' {
        if let Some(p) = repo.get(id_or_name).await.map_err(|e| e.to_string())? {
            return Ok(p);
        }
    }
    for p in repo.list().await.map_err(|e| e.to_string())? {
        if p.name == id_or_name {
            return Ok(p);
        }
    }
    Err(format!(
        "projeto `{id_or_name}` não existe; liste os projetos com \
         `ta-cli list-layers` (ou crie com `ta-cli seed-demo`)"
    ))
}

/// `exchange export --project <id|nome> --out <arquivo>`.
pub(crate) async fn exchange_export(
    dsn: Option<&str>,
    project: &str,
    out: &PathBuf,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let proj = resolve_existing_project(&pool, project).await?;
    let pid = proj.id.clone();

    // -- projeto (configurações do fluxo) -----------------------------------
    let settings = PgProjectRepo::new(pool.clone())
        .flow_settings(&pid)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("projeto `{}` sumiu durante o export", proj.name))?;
    let mut doc = ExchangeDoc::empty(ExchangeProject {
        name: proj.name.clone(),
        description: settings.description,
        active: settings.active,
        min_scale: settings.min_scale,
        max_scale: settings.max_scale,
        max_cells: settings.max_cells,
    });

    // -- fases ---------------------------------------------------------------
    let phases = PgPhaseRepo::new(pool.clone())
        .list_for_project(&pid)
        .await
        .map_err(|e| e.to_string())?;
    let phase_name: BTreeMap<String, String> = phases
        .iter()
        .map(|p| (p.id.clone(), p.name.clone()))
        .collect();
    doc.phases = phases
        .iter()
        .map(|p| ExchangePhase {
            name: p.name.clone(),
            description: p.description.clone(),
            cell_blocker: p.cell_blocker,
            sort_order: p.sort_order,
        })
        .collect();

    // -- cenas (vinculadas ao projeto) ---------------------------------------
    let scenes = PgSceneRepo::new(pool.clone())
        .list(Some(&pid))
        .await
        .map_err(|e| e.to_string())?;
    let mut scene_by_id: BTreeMap<String, String> = scenes
        .iter()
        .map(|s| (s.id.clone(), s.name.clone()))
        .collect();
    doc.scenes = scenes
        .iter()
        .map(|s| ExchangeScene {
            name: s.name.clone(),
            acquisition_on: s.acquisition_on.map(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default()
            }),
            footprint_wkt: s
                .footprint
                .as_ref()
                .map(|mp| geometry_to_wkt(&Geometry::from(mp.clone()))),
            metadata: s.metadata.clone(),
        })
        .collect();

    // -- AOI (camada + features) ---------------------------------------------
    let aois = PgAoiLayerRepo::new(pool.clone())
        .list_for_project(&pid)
        .await
        .map_err(|e| e.to_string())?;
    // mapa aoi_layer_id → nome da CAMADA (a grade referencia a AOI por nome).
    let mut aoi_layer_name: BTreeMap<String, String> = BTreeMap::new();
    for aoi in &aois {
        let layer = PgLayerRepo::new(pool.clone())
            .get(&aoi.layer_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("camada `{}` da AOI sumiu durante o export", aoi.layer_id))?;
        aoi_layer_name.insert(aoi.id.clone(), layer.name.clone());
        let features = PgFeatureRepo::new(pool.clone())
            .list(&aoi.layer_id)
            .await
            .map_err(|e| e.to_string())?;
        doc.aoi_layers.push(ExchangeAoiLayer {
            layer_name: layer.name.clone(),
            phase_name: aoi
                .phase_id
                .as_ref()
                .map(|id| phase_name.get(id).cloned().unwrap_or_else(|| id.clone())),
            id_column: aoi.aoi_id_column.clone(),
            features: features
                .iter()
                .map(|f| ExchangeFeature {
                    wkt: geometry_to_wkt(&f.geom),
                    attrs: f.attrs.clone(),
                })
                .collect(),
        });
    }

    // -- grades (células + bloqueios) -----------------------------------------
    let cell_layers = PgCellLayerRepo::new(pool.clone())
        .list_for_project(&pid)
        .await
        .map_err(|e| e.to_string())?;
    let cell_layer_name: BTreeMap<String, String> = cell_layers
        .iter()
        .map(|l| (l.id.clone(), l.name.clone()))
        .collect();
    for cl in &cell_layers {
        let cells = PgCellRepo::new(pool.clone())
            .list(&cl.id)
            .await
            .map_err(|e| e.to_string())?;
        let blocked = PgBlockedCellRepo::new(pool.clone())
            .list(&cl.id)
            .await
            .map_err(|e| e.to_string())?;
        doc.cell_layers.push(ExchangeCellLayer {
            name: cl.name.clone(),
            phase_name: cl
                .phase_id
                .as_ref()
                .map(|id| phase_name.get(id).cloned().unwrap_or_else(|| id.clone())),
            resolution: cl.resolution,
            srid: cl.srid,
            origin_x: cl.origin_x,
            origin_y: cl.origin_y,
            aoi_layer_name: aoi_layer_name.get(&cl.aoi_layer_id).cloned(),
            cells: cells
                .iter()
                .map(|c: &Cell| ExchangeCell {
                    cell_id: c.cell_id.clone(),
                    row_index: c.row_index,
                    col_index: c.col_index,
                    wkt: geometry_to_wkt(&Geometry::Polygon(c.geom.clone())),
                })
                .collect(),
            blocked: blocked
                .iter()
                .map(|b| ExchangeBlocked {
                    cell_id: b.cell_id.clone(),
                    reason: b.reason.clone(),
                })
                .collect(),
        });
    }

    // -- grupos e classes ------------------------------------------------------
    let groups = PgClassGroupRepo::new(pool.clone())
        .list_for_project(&pid)
        .await
        .map_err(|e| e.to_string())?;
    let group_name: BTreeMap<String, String> = groups
        .iter()
        .map(|g| (g.id.clone(), g.name.clone()))
        .collect();
    doc.class_groups = groups
        .iter()
        .map(|g| ExchangeClassGroup {
            name: g.name.clone(),
            sort_order: g.sort_order,
        })
        .collect();

    let classes = PgClassRepo::new(pool.clone())
        .list_for_project(&pid)
        .await
        .map_err(|e| e.to_string())?;
    let class_name: BTreeMap<String, String> = classes
        .iter()
        .map(|c| (c.id.clone(), c.name.clone()))
        .collect();
    doc.classes = classes
        .iter()
        .map(|c| ExchangeClass {
            name: c.name.clone(),
            group_name: c
                .class_group_id
                .as_ref()
                .map(|id| group_name.get(id).cloned().unwrap_or_else(|| id.clone())),
            parent_name: c
                .parent_id
                .as_ref()
                .map(|id| class_name.get(id).cloned().unwrap_or_else(|| id.clone())),
            color: c.color.clone(),
            sort_order: c.sort_order,
            is_active: c.is_active,
        })
        .collect();

    // -- rulesets + regras -----------------------------------------------------
    let rulesets = PgRuleSetRepo::new(pool.clone())
        .list_for_project(&pid)
        .await
        .map_err(|e| e.to_string())?;
    let active = PgRuleSetRepo::new(pool.clone())
        .find_active(&pid)
        .await
        .map_err(|e| e.to_string())?;
    // mapa ruleset_id → (nome, versão) para a proveniência dos rascunhos.
    let mut ruleset_ref: BTreeMap<String, (String, i32)> = BTreeMap::new();
    for rs in &rulesets {
        let rules = PgRuleRepo::new(pool.clone())
            .list_for_ruleset(&rs.id)
            .await
            .map_err(|e| e.to_string())?;
        ruleset_ref.insert(rs.id.clone(), (rs.name.clone(), rs.version));
        doc.rulesets.push(ExchangeRuleset {
            name: rs.name.clone(),
            version: rs.version,
            description: rs.description.clone(),
            is_active: active.as_ref().is_some_and(|a| a.id == rs.id),
            rules: rules
                .iter()
                .map(|r| ExchangeRule {
                    name: r.name.clone(),
                    class_name: class_name
                        .get(&r.class_id)
                        .cloned()
                        .unwrap_or_else(|| r.class_id.clone()),
                    priority: r.priority,
                    predicate: r.predicate.clone(),
                    is_active: r.is_active,
                })
                .collect(),
        });
    }

    // -- rascunhos (+ decisões) -------------------------------------------------
    let drafts = PgDraftLayerRepo::new(pool.clone())
        .list_for_project(&pid)
        .await
        .map_err(|e| e.to_string())?;
    for d in &drafts {
        let draft_repo = PgDraftLayerRepo::new(pool.clone());
        let decisions = draft_repo
            .list_cells(&d.id)
            .await
            .map_err(|e| e.to_string())?;
        doc.draft_layers.push(ExchangeDraftLayer {
            name: d.name.clone(),
            phase_name: d
                .phase_id
                .as_ref()
                .map(|id| phase_name.get(id).cloned().unwrap_or_else(|| id.clone())),
            cell_layer_name: d.cell_layer_id.as_ref().map(|id| {
                cell_layer_name
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.clone())
            }),
            state: d.state.as_str().to_owned(),
            cells: decisions
                .iter()
                .map(|dc| ExchangeDraftCell {
                    cell_id: dc.cell_id.clone(),
                    cell_layer_name: cell_layer_name
                        .get(&dc.cell_layer_id)
                        .cloned()
                        .unwrap_or_else(|| dc.cell_layer_id.clone()),
                    class_name: class_name
                        .get(&dc.class_id)
                        .cloned()
                        .unwrap_or_else(|| dc.class_id.clone()),
                    ruleset_name: ruleset_ref
                        .get(&dc.ruleset_id)
                        .map(|(n, _)| n.clone())
                        .unwrap_or_else(|| dc.ruleset_id.clone()),
                    ruleset_version: ruleset_ref
                        .get(&dc.ruleset_id)
                        .map(|(_, v)| *v)
                        .unwrap_or(0),
                    decided_on: Some(
                        dc.decided_on
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_default(),
                    ),
                })
                .collect(),
        });
    }

    // -- medidas (por grade; cena resolvida por nome) ----------------------------
    for cl in &cell_layers {
        let measures = PgTaskRepo::new(pool.clone())
            .list_measures(&cl.id, None)
            .await
            .map_err(|e| e.to_string())?;
        for m in &measures {
            if let Some(scene_id) = &m.scene_id {
                // Cena referenciada por medida mas não vinculada ao projeto:
                // entra na seção `scenes` também (senão o import não resolve).
                if !scene_by_id.contains_key(scene_id) {
                    if let Some(s) = PgSceneRepo::new(pool.clone())
                        .get(scene_id)
                        .await
                        .map_err(|e| e.to_string())?
                    {
                        scene_by_id.insert(s.id.clone(), s.name.clone());
                        doc.scenes.push(ExchangeScene {
                            name: s.name.clone(),
                            acquisition_on: s.acquisition_on.map(|t| {
                                t.format(&time::format_description::well_known::Rfc3339)
                                    .unwrap_or_default()
                            }),
                            footprint_wkt: s
                                .footprint
                                .as_ref()
                                .map(|mp| geometry_to_wkt(&Geometry::from(mp.clone()))),
                            metadata: s.metadata.clone(),
                        });
                    }
                }
            }
            doc.measures.push(ExchangeMeasure {
                cell_layer_name: cl.name.clone(),
                cell_id: m.cell_id.clone(),
                measure: m.measure.clone(),
                value: m.value,
                source: m.source.clone(),
                scene_name: m
                    .scene_id
                    .as_ref()
                    .and_then(|id| scene_by_id.get(id).cloned()),
            });
        }
    }

    // -- escreve o JSON ----------------------------------------------------------
    let counts = SectionCounts::of(&doc);
    let json = doc.to_json_pretty().map_err(|e| e.to_string())?;
    std::fs::write(out, &json)
        .map_err(|e| format!("não consegui escrever `{}`: {e}", out.display()))?;
    println!(
        "exchange exportado: {} ({} bytes)\nprojeto={}\nfases={}\taoi={}\tfeatures={}\tgrades={}\tcelulas={}\tbloqueadas={}\ngrupos={}\tclasses={}\trulesets={}\tregras={}\trascunhos={}\tdecisoes={}\tmedidas={}\tcenas={}",
        out.display(),
        json.len(),
        doc.project.name,
        counts.phases,
        counts.aoi_layers,
        counts.features,
        counts.cell_layers,
        counts.cells,
        counts.blocked,
        counts.class_groups,
        counts.classes,
        counts.rulesets,
        counts.rules,
        counts.draft_layers,
        counts.draft_cells,
        counts.measures,
        counts.scenes,
    );
    pool.close().await;
    Ok(())
}

/// Contagens por seção — compartilhadas pela saída do export e do import.
struct SectionCounts {
    phases: usize,
    aoi_layers: usize,
    features: usize,
    cell_layers: usize,
    cells: usize,
    blocked: usize,
    class_groups: usize,
    classes: usize,
    rulesets: usize,
    rules: usize,
    draft_layers: usize,
    draft_cells: usize,
    measures: usize,
    scenes: usize,
}

impl SectionCounts {
    fn of(doc: &ExchangeDoc) -> Self {
        Self {
            phases: doc.phases.len(),
            aoi_layers: doc.aoi_layers.len(),
            features: doc.aoi_layers.iter().map(|a| a.features.len()).sum(),
            cell_layers: doc.cell_layers.len(),
            cells: doc.cell_layers.iter().map(|c| c.cells.len()).sum(),
            blocked: doc.cell_layers.iter().map(|c| c.blocked.len()).sum(),
            class_groups: doc.class_groups.len(),
            classes: doc.classes.len(),
            rulesets: doc.rulesets.len(),
            rules: doc.rulesets.iter().map(|r| r.rules.len()).sum(),
            draft_layers: doc.draft_layers.len(),
            draft_cells: doc.draft_layers.iter().map(|d| d.cells.len()).sum(),
            measures: doc.measures.len(),
            scenes: doc.scenes.len(),
        }
    }
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

/// `exchange import --in <arquivo> --into <novo-nome-projeto>`.
///
/// RECUSA nome de projeto existente (sem merge nesta fase). ATÔMICO: projeto
/// e TODAS as seções são gravados numa ÚNICA transação — falha em qualquer
/// seção não deixa nada no banco (nem projeto parcial) e o mesmo `--into`
/// pode ser repetido imediatamente.
pub(crate) async fn exchange_import(
    dsn: Option<&str>,
    input: &PathBuf,
    into: &str,
) -> Result<(), String> {
    let text = std::fs::read_to_string(input)
        .map_err(|e| format!("não consegui ler `{}`: {e}", input.display()))?;
    let doc = ExchangeDoc::from_json(&text).map_err(|e: ExchangeError| e.to_string())?;
    if into.trim().is_empty() {
        return Err("--into vazio: informe o nome do projeto NOVO a criar".to_owned());
    }

    let pool = crate::open_pool(dsn).await?;
    let projects = PgProjectRepo::new(pool.clone());
    if projects
        .list()
        .await
        .map_err(|e| e.to_string())?
        .iter()
        .any(|p| p.name == into)
    {
        return Err(format!(
            "projeto `{into}` já existe: o import NÃO faz merge nesta fase; \
             escolha outro nome em --into ou apague o projeto existente antes \
             (`project delete --project {into} --yes`)"
        ));
    }

    // -- transação ÚNICA: projeto + todas as seções ---------------------------
    //    Qualquer erro descarta a transação inteira (rollback ao sair).
    let mut tx = pool.begin().await.map_err(tx_err)?;
    let created = PgProjectRepo::create_on(
        &mut *tx,
        &ta_core::project::Project {
            id: String::new(),
            name: into.to_owned(),
            srid: TARGET_SRID,
            created_at: time::OffsetDateTime::now_utc(),
        },
    )
    .await
    .map_err(|e| format!("criar projeto `{into}`: {e}"))?;
    PgProjectRepo::apply_flow_settings_on(
        &mut *tx,
        &created.id,
        &ProjectFlowSettings {
            active: doc.project.active,
            min_scale: doc.project.min_scale,
            max_scale: doc.project.max_scale,
            max_cells: doc.project.max_cells,
            description: doc.project.description.clone(),
        },
    )
    .await
    .map_err(|e| format!("criar projeto `{into}`: {e}"))?;

    match import_sections(&mut tx, &created.id, &doc).await {
        Ok((counts, warnings)) => {
            tx.commit().await.map_err(tx_err)?;
            // Estatísticas do planner DEPOIS do commit (M5): o insert de
            // células entra numa transação única e o ANALYZE não pode vê-las
            // antes do commit. Best-effort com aviso — o import já está
            // gravado e o autovacuum acaba coletando as estatísticas.
            if counts.cells > 0 {
                if let Err(e) = PgCellRepo::analyze_cells_on(&pool).await {
                    println!(
                        "aviso: ANALYZE cells não executou ({e}); a viewport pode \
                         ficar mais lenta até o autovacuum coletar estatísticas"
                    );
                }
            }
            println!(
                "exchange importado: projeto `{into}` ({})\n\
                 fases={}\taoi={}\tfeatures={}\tgrades={}\tcelulas={}\tbloqueadas={}\n\
                 grupos={}\tclasses={}\trulesets={}\tregras={}\trascunhos={}\tdecisoes={}\n\
                 medidas={}\tcenas={}",
                created.id,
                counts.phases,
                counts.aoi_layers,
                counts.features,
                counts.cell_layers,
                counts.cells,
                counts.blocked,
                counts.class_groups,
                counts.classes,
                counts.rulesets,
                counts.rules,
                counts.draft_layers,
                counts.draft_cells,
                counts.measures,
                counts.scenes,
            );
            for w in &warnings {
                println!("aviso: {w}");
            }
            pool.close().await;
            Ok(())
        }
        Err(e) => {
            // Defesa em profundidade: o rollback acontece ao descartar a
            // transação, mas a mensagem cita o caminho de saída caso algo
            // reste (banco fora da garantia, crash entre commit e saída).
            let _ = tx.rollback().await;
            pool.close().await;
            Err(format!(
                "{e}\nimport é ATÔMICO: nada foi gravado — o projeto `{into}` não \
                 ficou no banco e o mesmo --into pode ser repetido; se por qualquer \
                 motivo algo restar, remova com `project delete --project {into} --yes`"
            ))
        }
    }
}

/// Grava TODAS as seções na ordem de dependência, sobre a MESMA conexão de
/// transação do chamador (o import é atômico de ponta a ponta). Lê e escreve
/// tudo por essa conexão: o que a seção anterior gravou só é visível dentro
/// dela. Devolve as contagens e os avisos de reaproveitamento (cena global
/// com footprint/metadata divergentes do documento).
async fn import_sections(
    conn: &mut sqlx::PgConnection,
    pid: &str,
    doc: &ExchangeDoc,
) -> Result<(SectionCounts, Vec<String>), String> {
    let mut counts = SectionCounts {
        phases: 0,
        aoi_layers: 0,
        features: 0,
        cell_layers: 0,
        cells: 0,
        blocked: 0,
        class_groups: 0,
        classes: 0,
        rulesets: 0,
        rules: 0,
        draft_layers: 0,
        draft_cells: 0,
        measures: 0,
        scenes: 0,
    };
    let mut warnings: Vec<String> = Vec::new();

    // -- cenas (+ vínculos com o projeto) — antes das medidas, que as
    //    referenciam por nome. Cena existente (nome é global) é REAPROVEITADA
    //    sem alteração; divergência de footprint/metadata vira AVISO.
    {
        for (i, s) in doc.scenes.iter().enumerate() {
            let where_ = ctx("scenes", i, &s.name);
            // Parse ANTES do reaproveitamento: o documento inválido é erro
            // mesmo quando a cena já existe (e a comparação precisa do valor).
            let acquisition = s
                .acquisition_on
                .as_deref()
                .map(|v| parse_iso8601(&where_, v))
                .transpose()
                .map_err(sec("scenes"))?;
            let footprint = match &s.footprint_wkt {
                None => None,
                Some(wkt) => Some(scene_footprint_from_wkt(&where_, wkt).map_err(sec("scenes"))?),
            };
            let metadata = if s.metadata.is_null() {
                serde_json::json!({})
            } else {
                s.metadata.clone()
            };
            let scene_id = match PgSceneRepo::get_by_name_on(&mut *conn, &s.name)
                .await
                .map_err(sec("scenes"))?
            {
                Some(existing) => {
                    let mut divergiu: Vec<&str> = Vec::new();
                    if footprint_wkt_normalizado(&existing.footprint)
                        != footprint_wkt_normalizado(&footprint)
                    {
                        divergiu.push("footprint");
                    }
                    if existing.metadata != metadata {
                        divergiu.push("metadata");
                    }
                    if !divergiu.is_empty() {
                        warnings.push(format!(
                            "cena `{}` já existia com {} diferentes — \
                                 reaproveitada sem alteração",
                            s.name,
                            divergiu.join("/")
                        ));
                    }
                    existing.id
                }
                None => {
                    let scene = ta_core::scene::Scene {
                        id: String::new(),
                        name: s.name.clone(),
                        acquisition_on: acquisition,
                        footprint,
                        metadata,
                        created_on: time::OffsetDateTime::now_utc(),
                        updated_on: time::OffsetDateTime::now_utc(),
                    };
                    PgSceneRepo::create_on(&mut *conn, &scene)
                        .await
                        .map_err(sec("scenes"))?
                        .id
                }
            };
            let linked = PgSceneRepo::link_on(&mut *conn, pid, &scene_id)
                .await
                .map_err(sec("scenes"))?;
            let _ = linked;
            counts.scenes += 1;
        }
    }

    // -- fases -------------------------------------------------------------------
    let mut phase_ids: BTreeMap<String, String> = BTreeMap::new();
    {
        for p in &doc.phases {
            let mut phase = Phase::new(pid, &p.name);
            phase.description = p.description.clone();
            phase.cell_blocker = p.cell_blocker;
            phase.sort_order = p.sort_order;
            let saved = PgPhaseRepo::create_on(&mut *conn, &phase)
                .await
                .map_err(sec("phases"))?;
            phase_ids.insert(p.name.clone(), saved.id);
            counts.phases += 1;
        }
    }

    // -- AOI: camada + features + vínculo ------------------------------------------
    //    mapa camada(AOI) → aoi_layers.id — a grade referencia a AOI por nome.
    let mut aoi_by_layer_name: BTreeMap<String, String> = BTreeMap::new();
    {
        for (i, a) in doc.aoi_layers.iter().enumerate() {
            let where_ = ctx("aoi_layers", i, &a.layer_name);
            let phase_id = resolve_phase(&phase_ids, &where_, a.phase_name.as_deref())?;
            // GeomType dominante da primeira feição (fallback `geometry`).
            let geom_type = geom_type_of(&a.features, &where_)?;
            let layer = Layer {
                id: String::new(),
                project_id: pid.to_owned(),
                name: a.layer_name.clone(),
                geom_type,
                srid: TARGET_SRID,
                // O exchange não carrega a origem do arquivo — a proveniência
                // do import é registrada no source_uri (documentado).
                source_uri: "exchange-import".to_owned(),
            };
            let saved_layer = PgLayerRepo::add_on(&mut *conn, &layer)
                .await
                .map_err(sec("aoi_layers"))?;
            let features: Vec<Feature> = a
                .features
                .iter()
                .enumerate()
                .map(|(j, f)| {
                    Ok(Feature::new(
                        parse_wkt(&format!("{where_}.features[{j}]"), &f.wkt)
                            .map_err(sec("aoi_layers"))?,
                        if f.attrs.is_null() {
                            serde_json::json!({})
                        } else {
                            f.attrs.clone()
                        },
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let inserted = PgFeatureRepo::insert_on(&mut *conn, &saved_layer.id, &features)
                .await
                .map_err(sec("aoi_layers"))?;
            let aoi = ta_core::aoi::AoiLayer::new(pid, &saved_layer.id, a.id_column.clone())
                .with_phase(phase_id.as_deref());
            let saved_aoi = PgAoiLayerRepo::create_on(&mut *conn, &aoi)
                .await
                .map_err(sec("aoi_layers"))?;
            aoi_by_layer_name.insert(a.layer_name.clone(), saved_aoi.id.clone());
            counts.aoi_layers += 1;
            counts.features += usize::try_from(inserted).unwrap_or(0);
        }
    }

    // -- grades: camada + células (bulk, WKT 4674 direto) + bloqueios --------------
    let mut cell_layer_ids: BTreeMap<String, String> = BTreeMap::new();
    {
        for (i, c) in doc.cell_layers.iter().enumerate() {
            let where_ = ctx("cell_layers", i, &c.name);
            let phase_id = resolve_phase(&phase_ids, &where_, c.phase_name.as_deref())?;
            let aoi_id = match &c.aoi_layer_name {
                Some(name) => aoi_by_layer_name.get(name).cloned().ok_or_else(|| {
                    format!(
                        "seção `cell_layers`: grade `{}` referencia aoi_layer_name `{}`, \
                         mas nenhuma AOI com esse nome foi criada; a seção `aoi_layers` \
                         precisa conter a camada da grade",
                        c.name, name
                    )
                })?,
                None => match aoi_by_layer_name.values().collect::<Vec<_>>().as_slice() {
                    [only] => (*only).clone(),
                    _ => {
                        return Err(format!(
                            "seção `cell_layers`: grade `{}` sem aoi_layer_name e o \
                             projeto tem {} AOI(s); informe `aoi_layer_name` na grade",
                            c.name,
                            aoi_by_layer_name.len()
                        ));
                    }
                },
            };
            let spec = ta_core::cell::GridSpec::new(c.resolution, c.origin_x, c.origin_y, c.srid);
            let mut layer = ta_core::cell::CellLayer::new(pid, &aoi_id, &c.name, &spec);
            layer.phase_id = phase_id;
            let saved = PgCellLayerRepo::create_on(&mut *conn, &layer)
                .await
                .map_err(sec("cell_layers"))?;
            let cells: Vec<Cell> = c
                .cells
                .iter()
                .enumerate()
                .map(|(j, cell)| {
                    Ok(Cell {
                        id: 0,
                        cell_layer_id: saved.id.clone(),
                        cell_id: cell.cell_id.clone(),
                        geom: match parse_wkt(&format!("{where_}.cells[{j}]"), &cell.wkt)
                            .map_err(sec("cell_layers"))?
                        {
                            Geometry::Polygon(p) => p,
                            _ => {
                                return Err(format!(
                                    "seção `cell_layers`: célula `{}` de `{}` não é \
                                     POLYGON (células são polígonos)",
                                    cell.cell_id, c.name
                                ));
                            }
                        },
                        row_index: cell.row_index,
                        col_index: cell.col_index,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let inserted = PgCellRepo::import_cells_4674_on(&mut *conn, &saved.id, &cells)
                .await
                .map_err(sec("cell_layers"))?;
            for b in &c.blocked {
                PgBlockedCellRepo::block_on(
                    &mut *conn,
                    &ta_core::cell::BlockedCell {
                        id: 0,
                        cell_layer_id: saved.id.clone(),
                        cell_id: b.cell_id.clone(),
                        reason: b.reason.clone(),
                        created_at: time::OffsetDateTime::now_utc(),
                    },
                )
                .await
                .map_err(sec("cell_layers"))?;
                counts.blocked += 1;
            }
            cell_layer_ids.insert(c.name.clone(), saved.id);
            counts.cell_layers += 1;
            counts.cells += usize::try_from(inserted).unwrap_or(0);
        }
    }

    // -- grupos de classes ----------------------------------------------------------
    let mut group_ids: BTreeMap<String, String> = BTreeMap::new();
    {
        for g in &doc.class_groups {
            let mut group = ClassGroup::new(pid, &g.name);
            group.sort_order = g.sort_order;
            let saved = PgClassGroupRepo::create_on(&mut *conn, &group)
                .await
                .map_err(sec("class_groups"))?;
            group_ids.insert(g.name.clone(), saved.id);
            counts.class_groups += 1;
        }
    }

    // -- classes: mães resolvidas em ORDEM TOPOLOGICA (FK classes.parent_id
    //    exige a mãe antes da filha; ciclo ou mãe inexistente no documento é
    //    erro antes do banco) ---
    let mut class_ids: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut pending: Vec<&ExchangeClass> = doc.classes.iter().collect();
        let mut pass = 0;
        while !pending.is_empty() {
            pass += 1;
            if pass > pending.len() + 1 {
                let ciclo: Vec<String> = pending.iter().map(|c| c.name.clone()).collect();
                return Err(format!(
                    "seção `classes`: parent_name com ciclo ou mãe inexistente entre {}; \
                     a cadeia de mães precisa terminar numa classe raiz (sem parent_name)",
                    ciclo.join(", ")
                ));
            }
            let mut still_pending: Vec<&ExchangeClass> = Vec::new();
            for c in &pending {
                let ready = match &c.parent_name {
                    None => true,
                    Some(parent) => class_ids.contains_key(parent),
                };
                if !ready {
                    still_pending.push(c);
                    continue;
                }
                let mut class = Class::new(pid, &c.name);
                class.class_group_id = match &c.group_name {
                    None => None,
                    Some(name) => Some(group_ids.get(name).cloned().ok_or_else(|| {
                        format!(
                            "seção `classes`: classe `{}` referencia group_name `{}`, \
                             mas nenhum grupo com esse nome existe no documento \
                             (seção `class_groups`)",
                            c.name, name
                        )
                    })?),
                };
                class.parent_id = match &c.parent_name {
                    None => None,
                    Some(name) => Some(class_ids.get(name).cloned().ok_or_else(|| {
                        format!(
                            "seção `classes`: mãe `{name}` de `{}` não resolvida \
                             (ciclo ou mãe inexistente na seção `classes`)",
                            c.name
                        )
                    })?),
                };
                class.color = c.color.clone();
                class.sort_order = c.sort_order;
                class.is_active = c.is_active;
                let saved = PgClassRepo::create_on(&mut *conn, &class)
                    .await
                    .map_err(sec("classes"))?;
                class_ids.insert(c.name.clone(), saved.id);
                counts.classes += 1;
            }
            pending = still_pending;
        }
    }

    // -- rulesets (+ regras): predicado validado ANTES de gravar --------------------
    let mut ruleset_ids: BTreeMap<(String, i32), String> = BTreeMap::new();
    {
        let mut active_first: Option<String> = None;
        for rs in doc.rulesets.iter() {
            let mut ruleset = RuleSet::new(pid, &rs.name);
            ruleset.version = rs.version;
            ruleset.description = rs.description.clone();
            let saved = PgRuleSetRepo::insert_with_version_on(&mut *conn, &ruleset)
                .await
                .map_err(sec("rulesets"))?;
            for r in &rs.rules {
                let class_id = class_ids.get(&r.class_name).cloned().ok_or_else(|| {
                    format!(
                        "seção `rulesets`: regra `{}` do ruleset `{}` (v{}) decide a \
                         classe `{}`, mas nenhuma classe com esse nome existe no \
                         documento (seção `classes`)",
                        r.name, rs.name, rs.version, r.class_name
                    )
                })?;
                if let Err(e) = parse_predicate(&r.predicate) {
                    return Err(format!(
                        "seção `rulesets`: regra `{name}` do ruleset `{rs}` (v{v}): \
                         predicado inválido `{p}`: {e}",
                        name = r.name,
                        rs = rs.name,
                        v = rs.version,
                        p = r.predicate
                    ));
                }
                let mut rule = Rule::new(&saved.id, &class_id, &r.name, &r.predicate);
                rule.priority = r.priority;
                rule.is_active = r.is_active;
                PgRuleRepo::create_on(&mut *conn, &rule)
                    .await
                    .map_err(sec("rulesets"))?;
                counts.rules += 1;
            }
            ruleset_ids.insert((rs.name.clone(), rs.version), saved.id.clone());
            if rs.is_active && active_first.is_none() {
                active_first = Some(saved.id.clone());
            }
            counts.rulesets += 1;
        }
        // No máximo um ativo por projeto (índice parcial único): o PRIMEIRO
        // ativo do documento vence; demais são ignorados (política documentada).
        if let Some(id) = active_first {
            PgRuleSetRepo::activate_on(&mut *conn, &id)
                .await
                .map_err(sec("rulesets"))?;
        }
    }

    // -- rascunhos (+ decisões) --------------------------------------------------------
    {
        for (i, d) in doc.draft_layers.iter().enumerate() {
            let where_ = ctx("draft_layers", i, &d.name);
            let state: DraftState = d
                .state
                .parse()
                .map_err(|_| {
                    format!(
                        "seção `draft_layers`: rascunho `{}` com state `{}` inválido \
                         (use `draft` ou `consolidated`)",
                        d.name, d.state
                    )
                })
                .map_err(sec("draft_layers"))?;
            let phase_id = resolve_phase(&phase_ids, &where_, d.phase_name.as_deref())?;
            let cell_layer_id = match &d.cell_layer_name {
                None => None,
                Some(name) => Some(cell_layer_ids.get(name).cloned().ok_or_else(|| {
                    format!(
                        "seção `draft_layers`: rascunho `{}` referencia cell_layer_name \
                         `{}`, mas nenhuma grade com esse nome existe no documento \
                         (seção `cell_layers`)",
                        d.name, name
                    )
                })?),
            };
            let mut draft = DraftLayer::new(pid, &d.name);
            draft.state = state;
            draft.phase_id = phase_id;
            draft.cell_layer_id = cell_layer_id.clone();
            let saved = PgDraftLayerRepo::create_on(&mut *conn, &draft)
                .await
                .map_err(sec("draft_layers"))?;
            let mut rows = Vec::with_capacity(d.cells.len());
            for (j, dc) in d.cells.iter().enumerate() {
                let where_dc = format!("{where_}.cells[{j}]");
                let layer_id = cell_layer_ids
                    .get(&dc.cell_layer_name)
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "seção `draft_layers`: decisão `{where_dc}` referencia \
                             cell_layer_name `{}`, inexistente no documento",
                            dc.cell_layer_name
                        )
                    })?;
                let class_id = class_ids.get(&dc.class_name).cloned().ok_or_else(|| {
                    format!(
                        "seção `draft_layers`: decisão `{where_dc}` decide a classe `{}`, \
                         inexistente no documento (seção `classes`)",
                        dc.class_name
                    )
                })?;
                let ruleset_id = ruleset_ids
                    .get(&(dc.ruleset_name.clone(), dc.ruleset_version))
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "seção `draft_layers`: decisão `{where_dc}` referencia o \
                             ruleset `{}` v{}, inexistente no documento (seção `rulesets`)",
                            dc.ruleset_name, dc.ruleset_version
                        )
                    })?;
                // Célula validada NA MESMA transação (a seção `cell_layers`
                // commitou dentro dela, mas ainda não no banco).
                if !PgCellRepo::exists_on(&mut *conn, &layer_id, &dc.cell_id)
                    .await
                    .map_err(sec("draft_layers"))?
                {
                    return Err(format!(
                        "seção `draft_layers`: decisão `{where_dc}` aponta a célula `{}`, \
                         que não existe na grade `{}` do documento",
                        dc.cell_id, dc.cell_layer_name
                    ));
                }
                rows.push(ta_core::classification::DraftCell {
                    draft_layer_id: saved.id.clone(),
                    cell_layer_id: layer_id,
                    cell_id: dc.cell_id.clone(),
                    class_id,
                    ruleset_id,
                    decided_on: match &dc.decided_on {
                        None => time::OffsetDateTime::now_utc(),
                        Some(v) => parse_iso8601(&where_dc, v).map_err(sec("draft_layers"))?,
                    },
                });
            }
            let written = PgTaskRepo::import_draft_cells_on(&mut *conn, &saved.id, &rows)
                .await
                .map_err(sec("draft_layers"))?;
            counts.draft_layers += 1;
            counts.draft_cells += usize::try_from(written).unwrap_or(0);
        }
    }

    // -- medidas (upsert; last-write-wins) ------------------------------------------------
    {
        for (i, m) in doc.measures.iter().enumerate() {
            let where_ = ctx("measures", i, &m.cell_layer_name);
            let layer_id = cell_layer_ids
                .get(&m.cell_layer_name)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "seção `measures`: medida `{where_}` aponta a grade `{}`, \
                         inexistente no documento (seção `cell_layers`)",
                        m.cell_layer_name
                    )
                })?;
            if !PgCellRepo::exists_on(&mut *conn, &layer_id, &m.cell_id)
                .await
                .map_err(sec("measures"))?
            {
                return Err(format!(
                    "seção `measures`: medida `{where_}` aponta a célula `{}`, que \
                     não existe na grade `{}` do documento",
                    m.cell_id, m.cell_layer_name
                ));
            }
            let scene_id = match &m.scene_name {
                None => None,
                Some(name) => Some(
                    PgSceneRepo::get_by_name_on(&mut *conn, name)
                        .await
                        .map_err(sec("measures"))?
                        .map(|s| s.id)
                        .ok_or_else(|| {
                            format!(
                                "seção `measures`: medida `{where_}` referencia scene_name \
                             `{}`, inexistente no documento (seção `scenes`)",
                                name
                            )
                        })?,
                ),
            };
            let rows = vec![ta_core::classification::CellMeasure {
                cell_layer_id: layer_id,
                cell_id: m.cell_id.clone(),
                measure: m.measure.clone(),
                value: m.value,
                source: m.source.clone(),
                scene_id,
                updated_on: time::OffsetDateTime::now_utc(),
            }];
            let written = PgTaskRepo::upsert_measures_on(&mut *conn, &rows)
                .await
                .map_err(sec("measures"))?;
            counts.measures += usize::try_from(written).unwrap_or(0);
        }
    }

    Ok((counts, warnings))
}

/// Footprint normalizado em WKT canônico (o MESMO serializador dos dois
/// lados) — a comparação de reaproveitamento de cena não pode ser sensível à
/// formatação do WKT do documento (`None` = sem footprint dos dois lados).
fn footprint_wkt_normalizado(fp: &Option<MultiPolygon<f64>>) -> Option<String> {
    fp.as_ref()
        .map(|mp| geometry_to_wkt(&Geometry::from(mp.clone())))
}

/// Contexto de erro por item (`seção[i] (nome)`) — o "onde" acionável.
fn ctx(section: &str, index: usize, name: &str) -> String {
    format!("{section}[{index}] (`{name}`)")
}

/// GeomType dominante da primeira feição (fallback `geometry` — o
/// `GeometryCollection` é gravado como `geometry` no CHECK do schema).
fn geom_type_of(features: &[ExchangeFeature], where_: &str) -> Result<GeomType, String> {
    let Some(first) = features.first() else {
        return Ok(GeomType::GeometryCollection);
    };
    Ok(
        match parse_wkt(where_, &first.wkt).map_err(sec("aoi_layers"))? {
            Geometry::Point(_) => GeomType::Point,
            Geometry::LineString(_) | Geometry::Line(_) => GeomType::LineString,
            Geometry::Polygon(_) => GeomType::Polygon,
            Geometry::MultiPoint(_) => GeomType::MultiPoint,
            Geometry::MultiLineString(_) => GeomType::MultiLineString,
            Geometry::MultiPolygon(_) => GeomType::MultiPolygon,
            _ => GeomType::GeometryCollection,
        },
    )
}

/// Prefixa a seção culpada num erro de banco/transação (genérico no tipo do
/// erro — `ta_db::Error`, `ExchangeError`, `sqlx::Error`).
fn sec<E: std::fmt::Display>(section: &str) -> impl Fn(E) -> String + '_ {
    move |e| format!("seção `{section}`: {e}")
}

fn tx_err(e: sqlx::Error) -> String {
    e.to_string()
}

/// Resolve `phase_name` (referência opcional por nome) → id, com erro acionável.
fn resolve_phase(
    phase_ids: &BTreeMap<String, String>,
    where_: &str,
    phase_name: Option<&str>,
) -> Result<Option<String>, String> {
    match phase_name {
        None => Ok(None),
        Some(name) => Ok(Some(phase_ids.get(name).cloned().ok_or_else(|| {
            format!(
                "`{where_}` referencia phase_name `{name}`, mas nenhuma fase com esse \
                 nome existe no documento (seção `phases`)"
            )
        })?)),
    }
}
