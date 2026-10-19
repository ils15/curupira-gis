//! Export em thread de fundo — a UI nunca bloqueia em I/O de arquivo
//! (mesmo padrão do [`crate::import_job`]).
//!
//! Formato do MVP: **GeoJSON** (escrita pura via `ta-gis`, sem GDAL na
//! thread). A origem dos dados é a sessão em memória (features já em 4674).

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use ta_core::feature::Feature;
use ta_gis::writer::{WriteFormat, WriteOptions, write_vector};

/// Mensagens do worker de export para a UI.
#[derive(Debug)]
pub enum ExportMsg {
    /// Concluído: número de features gravadas.
    Done(usize),
    /// Falha com mensagem acionável (vira banner na UI).
    Failed(String),
}

/// Job ativo na UI (canal de resposta).
pub struct ExportJob {
    pub rx: Receiver<ExportMsg>,
}

/// Sobe a thread de export (GeoJSON).
pub fn spawn_export(features: Vec<Feature>, path: PathBuf) -> ExportJob {
    let (tx, rx) = channel();
    let spawned = std::thread::Builder::new()
        .name("ta-export".into())
        .spawn(move || {
            match write_vector(
                &features,
                &path,
                WriteFormat::GeoJson,
                &WriteOptions::default(),
            ) {
                Ok(n) => {
                    let _ = tx.send(ExportMsg::Done(n));
                }
                Err(e) => {
                    let _ = tx.send(ExportMsg::Failed(e.to_string()));
                }
            }
        });
    if let Err(err) = spawned {
        // Sem thread, `tx` cai com a closure e `rx` desconecta (export sem
        // resposta). Falha de SO não derruba o processo.
        eprintln!("ta-gui: falha ao iniciar a thread de export: {err}");
    }
    ExportJob { rx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::Point;

    #[test]
    fn export_geojson_grava_e_conta() {
        let dir = std::env::temp_dir().join(format!(
            "curupira-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pts.geojson");
        let feats = (0..3)
            .map(|i| {
                Feature::new(
                    geo_types::Geometry::Point(Point::new(f64::from(i), f64::from(i))),
                    serde_json::json!({"i": i}),
                )
            })
            .collect();
        let job = spawn_export(feats, path.clone());
        match job.rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(ExportMsg::Done(n)) => assert_eq!(n, 3),
            Ok(other) => panic!("mensagem inesperada: {other:?}"),
            Err(e) => panic!("export não concluiu: {e}"),
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"type\": \"FeatureCollection\""),
            "GeoJSON esperado"
        );
        assert_eq!(text.matches("\"type\": \"Feature\"").count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
