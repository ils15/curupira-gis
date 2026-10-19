//! Subcomandos de **cenas** (Fase 4): `scene add/list/link/unlink/remove`.
//!
//! A cena é global (nome único no banco) e o vínculo com o projeto é
//! `project_scenes` (N:N). `scene remove` é destrutivo e exige `--yes`; sem
//! ele a CLI mostra a contagem de vínculos e medidas que a cena sustenta
//! (padrão de intenção explícita da Fase 3). Apagar a cena nunca apaga
//! medida — a proveniência desvincula (`ON DELETE SET NULL`).

use std::path::Path;

use ta_db::repo::PgProjectRepo;
use ta_db::repo_scene::{PgSceneRepo, SceneRepo};

use crate::resolve_project;

/// Resolve `--footprint`: texto WKT direto ou arquivo (WKT ou GeoJSON).
///
/// Geometrias aceitas: POLYGON/MULTIPOLYGON em EPSG:4674 (POLYGON simples é
/// envolvido em MultiPolygon). GeoJSON é tratado como 4674 — MESMA política
/// do `seed-demo` (o CRS de origem declarado é responsabilidade de quem
/// exporta; coordenada sem datum não entra).
///
/// Os dois fracassos são DISTINTOS (acionáveis): WKT que não parseia é
/// "não é WKT reconhecível"; WKT válido não-poligonal (ex.: POINT) é
/// `FootprintNotPolygonal` ("esperado POLYGON/MULTIPOLYGON, recebido POINT").
fn footprint_from_arg(arg: &str) -> Result<geo_types::MultiPolygon<f64>, String> {
    use ta_gis::exchange::{ExchangeError, scene_footprint_from_wkt};

    let context = "--footprint";
    // WKT válido de outra geometria é recusado NA HORA (não vira tentativa
    // de arquivo): a mensagem distingue "não é WKT" de "não é polígono".
    let nao_poligonal = |e: ExchangeError| match e {
        ExchangeError::FootprintNotPolygonal { context, got } => format!(
            "{context}: esperado POLYGON/MULTIPOLYGON, recebido {got}; \
             informe POLYGON ou MULTIPOLYGON em EPSG:4674"
        ),
        other => other.to_string(),
    };
    // 1) Tenta WKT direto (POLYGON/MULTIPOLYGON...).
    match scene_footprint_from_wkt(context, arg) {
        Ok(mp) => return Ok(mp),
        Err(e @ ExchangeError::FootprintNotPolygonal { .. }) => {
            return Err(nao_poligonal(e));
        }
        Err(_) => {} // não é WKT reconhecível — tenta arquivo abaixo
    }
    // 2) Arquivo no disco: lê e tenta WKT, depois GeoJSON (feature gdal).
    let path = Path::new(arg);
    if !path.is_file() {
        return Err(format!(
            "--footprint inválido `{arg}`: não é WKT reconhecível e o arquivo não existe; \
             informe `POLYGON((...))`/`MULTIPOLYGON((...))` em EPSG:4674 ou um \
             caminho .wkt/.geojson"
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("--footprint: não consegui ler `{arg}`: {e}"))?;
    match scene_footprint_from_wkt(context, &text) {
        Ok(mp) => return Ok(mp),
        Err(e @ ExchangeError::FootprintNotPolygonal { .. }) => {
            return Err(nao_poligonal(e));
        }
        Err(_) => {} // arquivo não é WKT — tenta como fonte vetorial abaixo
    }
    let data = ta_gis::reader::read_vector(
        path,
        &ta_gis::reader::ReadOptions {
            srid: Some(ta_db::spatial::TARGET_SRID),
            ..ta_gis::reader::ReadOptions::default()
        },
    )
    .map_err(|e| {
        format!(
            "--footprint `{arg}`: não parseou como WKT nem como fonte vetorial ({e}); \
             use POLYGON/MULTIPOLYGON em EPSG:4674"
        )
    })?;
    ta_flow::grid::multipolygon_from_features(&data.features).map_err(|e| e.to_string())
}

/// `scene add` — registra uma cena (global; nome é único).
pub(crate) async fn scene_add(
    dsn: Option<&str>,
    name: &str,
    acquisition: Option<String>,
    footprint: Option<String>,
    metadata: Option<String>,
) -> Result<(), String> {
    let mut scene = ta_core::scene::Scene::new(name);
    if let Some(acq) = acquisition {
        scene.acquisition_on = Some(
            ta_gis::exchange::parse_iso8601("--acquisition", &acq).map_err(|e| e.to_string())?,
        );
    }
    if let Some(fp) = footprint {
        scene.footprint = Some(footprint_from_arg(&fp)?);
    }
    if let Some(meta) = metadata {
        let value: serde_json::Value = serde_json::from_str(&meta)
            .map_err(|e| format!("--metadata não é JSON válido: {e}"))?;
        if !value.is_object() {
            return Err(format!(
                "--metadata precisa ser um objeto JSON (ex.: '{{\"k\":\"v\"}}'); recebi {value}"
            ));
        }
        scene.metadata = value;
    }

    let pool = crate::open_pool(dsn).await?;
    let saved = PgSceneRepo::new(pool.clone())
        .create(&scene)
        .await
        .map_err(|e| e.to_string())?;
    println!(
        "cena criada: {}\t{}\taquisicao={}\tfootprint={}",
        saved.id,
        saved.name,
        saved
            .acquisition_on
            .map(|t| t.to_string())
            .unwrap_or_else(|| "-".into()),
        if saved.footprint.is_some() {
            "sim"
        } else {
            "não"
        }
    );
    pool.close().await;
    Ok(())
}

/// `scene list` — cenas globais ou as de um projeto (--project).
pub(crate) async fn scene_list(dsn: Option<&str>, project: Option<String>) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let project_id = match project {
        Some(ref p) => {
            let proj = resolve_project(&PgProjectRepo::new(pool.clone()), p).await?;
            Some(proj.id)
        }
        None => None,
    };
    let scenes = PgSceneRepo::new(pool.clone())
        .list(project_id.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    if scenes.is_empty() {
        match project {
            Some(p) => println!("(nenhuma cena vinculada ao projeto `{p}`)"),
            None => println!("(nenhuma cena registrada)"),
        }
    }
    for s in &scenes {
        println!(
            "{}\t{}\taquisicao={}\tfootprint={}\tmetadata={}",
            s.id,
            s.name,
            s.acquisition_on
                .map(|t| t.to_string())
                .unwrap_or_else(|| "-".into()),
            if s.footprint.is_some() { "sim" } else { "não" },
            s.metadata
        );
    }
    pool.close().await;
    Ok(())
}

/// Resolve cena por NOME com erro acionável (`scene não existe` → como criar).
async fn scene_id_by_name(pool: &sqlx::PgPool, name: &str) -> Result<String, String> {
    PgSceneRepo::new(pool.clone())
        .get_by_name(name)
        .await
        .map_err(|e| e.to_string())?
        .map(|s| s.id)
        .ok_or_else(|| {
            format!(
                "cena `{name}` não existe; crie com `ta-cli scene add --name {name}` \
                 ou liste as existentes com `ta-cli scene list`"
            )
        })
}

/// `scene link` — vincula a cena ao projeto (idempotente).
pub(crate) async fn scene_link(
    dsn: Option<&str>,
    project: &str,
    scene: &str,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let proj = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let scene_id = scene_id_by_name(&pool, scene).await?;
    let linked = PgSceneRepo::new(pool.clone())
        .link(&proj.id, &scene_id)
        .await
        .map_err(|e| e.to_string())?;
    if linked {
        println!("cena vinculada: {scene}\tprojeto={}", proj.name);
    } else {
        println!("cena já estava vinculada: {scene}\tprojeto={}", proj.name);
    }
    pool.close().await;
    Ok(())
}

/// `scene unlink` — desvincula a cena do projeto (a cena continua existindo).
pub(crate) async fn scene_unlink(
    dsn: Option<&str>,
    project: &str,
    scene: &str,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let proj = resolve_project(&PgProjectRepo::new(pool.clone()), project).await?;
    let scene_id = scene_id_by_name(&pool, scene).await?;
    let unlinked = PgSceneRepo::new(pool.clone())
        .unlink(&proj.id, &scene_id)
        .await
        .map_err(|e| e.to_string())?;
    if unlinked {
        println!("cena desvinculada: {scene}\tprojeto={}", proj.name);
    } else {
        println!(
            "cena não estava vinculada ao projeto `{}`: {scene}",
            proj.name
        );
    }
    pool.close().await;
    Ok(())
}

/// `scene remove` — apaga a cena (destrutivo; exige --yes). Sem --yes mostra
/// as contagens (projetos vinculados, medidas com a proveniência) e recusa.
pub(crate) async fn scene_remove(dsn: Option<&str>, name: &str, yes: bool) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let repo = PgSceneRepo::new(pool.clone());
    let scene = repo
        .get_by_name(name)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("cena `{name}` não existe; veja `ta-cli scene list`"))?;
    let projects = repo
        .projects_using(&scene.id)
        .await
        .map_err(|e| e.to_string())?;
    let measures = repo
        .measures_using(&scene.id)
        .await
        .map_err(|e| e.to_string())?;
    if !yes {
        return Err(format!(
            "recusado sem --yes: apagar a cena `{name}` desvincula {} projeto(s) ({}) \
             e remove a proveniência de {measures} medida(s) (as medidas PERMANECEM, \
             sem cena); repita com --yes para confirmar",
            projects.len(),
            if projects.is_empty() {
                "-".to_owned()
            } else {
                projects.join(", ")
            }
        ));
    }
    let removed = repo.delete(&scene.id).await.map_err(|e| e.to_string())?;
    println!(
        "cena removida: {name}\tvinculos_cascade={}\tmedidas_desvinculadas={measures}",
        projects.len()
    );
    let _ = removed;
    pool.close().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::footprint_from_arg;

    /// WKT válido mas não-poligonal (POINT) é recusado com a mensagem do
    /// `FootprintNotPolygonal` — DIFERENTE de WKT não reconhecível.
    #[test]
    fn footprint_point_rejeita_como_nao_poligonal() {
        let err = footprint_from_arg("POINT(-52 -12)").expect_err("POINT recusado");
        assert!(
            err.contains("esperado POLYGON/MULTIPOLYGON, recebido POINT"),
            "mensagem errada: {err}"
        );
    }

    /// Texto que não é WKT (nem arquivo) diz "não é WKT reconhecível".
    #[test]
    fn footprint_wkt_invalido_diz_nao_reconhecivel() {
        let err = footprint_from_arg("isto não é wkt ???").expect_err("lixo recusado");
        assert!(
            err.contains("não é WKT reconhecível"),
            "mensagem errada: {err}"
        );
    }

    /// POLYGON válido parseia (envolvido em MultiPolygon de 1 peça).
    #[test]
    fn footprint_polygon_aceito() {
        let mp = footprint_from_arg("POLYGON((0 0,1 0,1 1,0 0))").expect("polygon");
        assert_eq!(mp.0.len(), 1);
    }
}
