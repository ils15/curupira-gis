//! Import em thread de fundo — a UI nunca bloqueia (restrição da Fase 3).
//!
//! Fluxo: `std::thread` + canal mpsc; o pipeline chama `ta-gis` (detecção →
//! leitura → normalização para 4674) em etapas, emitindo `Stage` entre elas
//! para a barra de progresso. Cancelamento: verificado **entre etapas** (a
//! leitura de um arquivo é atômica dentro da thread — documentado, honesto).
//!
//! O import é **somente leitura da origem**: nada é escrito fora do banco/
//! memória da sessão (barreira clean-room: nenhum dado entra de outra via).

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use ta_core::feature::Feature;
use ta_core::layer::GeomType;
use ta_gis::normalizer::{self, NormalizationReport};
use ta_gis::raster::RasterMeta;
use ta_gis::reader::{CsvXyOptions, ReadOptions};

/// Mensagens do worker de import para a UI.
#[derive(Debug)]
pub enum ImportMsg {
    /// Etapa corrente (texto já i18n no ponto de partida — ver `spawn_import`).
    Stage(String),
    DoneVector {
        name: String,
        path: PathBuf,
        features: Vec<Feature>,
        geom: GeomType,
        bbox: [f64; 4],
        report: NormalizationReport,
    },
    DoneRaster {
        path: PathBuf,
        meta: RasterMeta,
    },
    /// Falha com mensagem acionável (mostrada em modal legível pela UI).
    Failed(String),
}

/// Job ativo na UI (barra de progresso + cancel).
pub struct ImportJob {
    pub rx: Receiver<ImportMsg>,
    pub stage: String,
    /// Cancelamento cooperativo (checado entre etapas).
    pub cancel: bool,
    /// Canal para o worker checar o cancel (Cliente → worker).
    pub cancel_tx: Sender<()>,
}

/// Import vetorial aguardando persistência (cadastro de camada/inserção).
#[derive(Debug)]
pub struct PendingImport {
    pub name: String,
    pub features: Vec<Feature>,
    pub geom: GeomType,
    pub bbox: [f64; 4],
    pub source: String,
    #[allow(dead_code)] // guardado para diagnóstico/relatório futuro
    pub srid: Option<u32>,
}

/// Sobe a thread de import para `path`.
///
/// `stage_reading`/`stage_normalizing` já vêm traduzidos do chamador (a UI
/// é a dona do i18n); o worker só repassa.
pub fn spawn_import(
    path: PathBuf,
    srid: Option<u32>,
    stage_reading: String,
    stage_normalizing: String,
) -> ImportJob {
    let (tx, rx) = std::sync::mpsc::channel::<ImportMsg>();
    let (cancel_tx, cancel_rx) = std::sync::mpsc::channel::<()>();
    let job_stage = stage_reading.clone();
    std::thread::Builder::new()
        .name("ta-import".into())
        .spawn(move || {
            run_import(
                &path,
                srid,
                cancel_rx,
                &tx,
                &stage_reading,
                &stage_normalizing,
            )
        })
        .expect("spawn da thread de import (recursos de thread do SO)");
    ImportJob {
        rx,
        stage: job_stage,
        cancel: false,
        cancel_tx,
    }
}

fn run_import(
    path: &Path,
    srid: Option<u32>,
    cancel_rx: Receiver<()>,
    tx: &Sender<ImportMsg>,
    stage_reading: &str,
    stage_normalizing: &str,
) {
    let _ = tx.send(ImportMsg::Stage(stage_reading.to_owned()));

    // Raster: só metadados (pixels ficam no arquivo — política da Fase 2).
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"))
    {
        match ta_gis::reader::read_raster_meta(path) {
            Ok(meta) => {
                let _ = tx.send(ImportMsg::DoneRaster {
                    path: path.to_path_buf(),
                    meta,
                });
            }
            Err(e) => {
                let _ = tx.send(ImportMsg::Failed(e.to_string()));
            }
        }
        return;
    }

    // Vetorial: detecta → lê → normaliza (mesma política da CLI).
    let opts = ReadOptions {
        srid,
        csv: CsvXyOptions {
            srid,
            ..CsvXyOptions::default()
        },
    };
    let data = match ta_gis::reader::read_vector(path, &opts) {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(ImportMsg::Failed(e.to_string()));
            return;
        }
    };
    if cancel_rx.try_recv().is_ok() {
        return; // cancelado entre etapas (comportamento documentado)
    }
    let _ = tx.send(ImportMsg::Stage(stage_normalizing.to_owned()));
    let input_srid = data.srid.or(srid);
    let (features, report) =
        match normalizer::normalize(data.features, input_srid, normalizer::TARGET_SRID) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(ImportMsg::Failed(e.to_string()));
                return;
            }
        };
    let Some(bbox) = normalizer::bbox_of(&features) else {
        let _ = tx.send(ImportMsg::Failed(
            "arquivo sem feições utilizáveis (vazio ou só geometria nula)".into(),
        ));
        return;
    };
    let name = data.layer_name;
    let geom = normalizer::dominant_geom_type(&features).unwrap_or(GeomType::GeometryCollection);
    let _ = tx.send(ImportMsg::DoneVector {
        name,
        path: path.to_path_buf(),
        features,
        geom,
        bbox,
        report,
    });
}
