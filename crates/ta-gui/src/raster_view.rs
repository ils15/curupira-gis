//! Banda GeoTIFF → imagem egui, com **downsample banda a banda**.
//!
//! Pipeline (decisão documentada da Fase 3):
//! 1. GDAL lê a banda ativa com `read_as` já decimado (janela cheia →
//!    shape alvo com `NearestNeighbour`) — o downsample pesado fica no GDAL;
//! 2. normalização min/max por banda → tons de cinza `Color32`;
//! 3. `ColorImage` ≤ `MAX_TEXTURE_DIM` vira textura egui uma vez
//!    (`MapViewEgui::ensure_raster_texture`); o "downsample para viewport"
//!    por frame é o próprio draw da textura escalada (O(viewport)).
//!
//! Nenhuma chamada GDAL acontece por frame — nem na thread da UI: a leitura
//! roda em thread de fundo ([`spawn_band_load`], mesmo padrão do
//! `import_job`: `std::thread` + mpsc, GDAL confinado à thread do worker) e
//! a UI pinta quando o canal entrega.
//!
//! GDAL não é thread-safe entre contextos, mas cada [`spawn_band_load`]
//! abre o próprio `Dataset` dentro da própria thread — sem compartilhamento.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use eframe::egui::Color32;
use gdal::Dataset;
use gdal::raster::ResampleAlg;

/// Erro local da visualização de raster (mensagem pronta para o usuário;
/// o GUI nunca mostra backtrace — apenas Display).
#[derive(Debug, thiserror::Error)]
#[error("raster: {0}")]
pub struct RasterError(pub String);

/// Result local do módulo.
pub type Result<T> = std::result::Result<T, RasterError>;

/// Lado máximo da textura (decima acima disso). 2048² RGBA ≈ 16 MiB.
pub const MAX_TEXTURE_DIM: usize = 2048;

/// Imagem de uma banda pronta para textura egui.
#[derive(Debug, Clone)]
pub struct RasterBandImage {
    /// Largura da imagem decimada (px).
    pub width: usize,
    /// Altura da imagem decimada (px).
    pub height: usize,
    /// Banda de origem (0-based) que gerou a imagem.
    pub band: usize,
    /// Pixels em tons de cinza normalizados.
    pub image: eframe::egui::ColorImage,
}

/// Lê a banda `band` (0-based) do raster, decimada para caber em
/// `MAX_TEXTURE_DIM`. Falha com erro legível (banda fora do intervalo,
/// arquivo sem banda, I/O GDAL).
pub fn load_band(path: &Path, band: usize) -> Result<RasterBandImage> {
    let ds = Dataset::open(path)
        .map_err(|e| RasterError(format!("abrir raster {}: {e}", path.display())))?;
    let (width, height) = ds.raster_size();
    if ds.raster_count() == 0 {
        return Err(RasterError(format!(
            "{} não tem bandas raster",
            path.display()
        )));
    }
    if band >= ds.raster_count() {
        return Err(RasterError(format!(
            "banda {} fora do intervalo (o arquivo tem {} banda(s))",
            band,
            ds.raster_count()
        )));
    }
    let gband = ds
        .rasterband(band + 1)
        .map_err(|e| RasterError(format!("banda {} de {}: {e}", band + 1, path.display())))?;

    // Downsample GDAL: janela cheia → shape alvo (nearest — rápido e
    // suficiente para preview; bilinear ficaria para a Fase 3b).
    let out_w = width.min(MAX_TEXTURE_DIM);
    let out_h = height.min(MAX_TEXTURE_DIM);
    let buf = gband
        .read_as::<u16>(
            (0, 0),
            (width, height),
            (out_w, out_h),
            Some(ResampleAlg::NearestNeighbour),
        )
        .map_err(|e| RasterError(format!("ler banda de {}: {e}", path.display())))?;

    let mut min = u16::MAX;
    let mut max = u16::MIN;
    for &v in buf.data() {
        min = min.min(v);
        max = max.max(v);
    }
    let span = f32::from(max).max(f32::from(min) + 1.0) - f32::from(min);
    let pixels: Vec<Color32> = buf
        .data()
        .iter()
        .map(|&v| {
            let g = (((f32::from(v) - f32::from(min)) / span) * 255.0).round() as u8;
            Color32::from_rgb(g, g, g)
        })
        .collect();

    // ColorImage::from_rgba_unmultiplied espera bytes RGBA planos.
    let flat = flatten_rgba(&pixels);
    let image = eframe::egui::ColorImage::from_rgba_unmultiplied([out_w, out_h], &flat);
    Ok(RasterBandImage {
        width: out_w,
        height: out_h,
        band,
        image,
    })
}

/// RGBA plano a partir de `Color32` (ColorImage precisa de &[u8]).
fn flatten_rgba(pixels: &[Color32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * 4);
    for p in pixels {
        out.extend_from_slice(&p.to_srgba_unmultiplied());
    }
    out
}

/// Mensagem do worker de banda para a UI.
#[derive(Debug)]
pub enum BandMsg {
    /// Leitura concluída — `Ok` traz a imagem pronta para textura; `Err`
    /// traz mensagem legível (o GUI nunca mostra backtrace — só Display).
    Done {
        /// Arquivo pedido (o chamador valida se o pedido ainda vale — o
        /// raster pode ter trocado durante a carga).
        path: PathBuf,
        /// Banda pedida (0-based).
        band: usize,
        /// Resultado da leitura GDAL (rode na thread do worker).
        result: Result<RasterBandImage>,
    },
}

/// Carregamento de banda em thread de fundo (a UI nunca bloqueia em GDAL).
pub struct BandLoadJob {
    /// Canal worker → UI (uma mensagem por job).
    pub rx: Receiver<BandMsg>,
    /// Arquivo pedido (dedup/diagnóstico na UI).
    pub path: PathBuf,
    /// Banda pedida (0-based).
    pub band: usize,
}

/// Sobe a thread de leitura GDAL para `(path, band)`.
///
/// Mesmo padrão do `import_job`: `std::thread` + mpsc; o `Dataset` é aberto
/// e fechado dentro da thread do worker (GDAL não atravessa a fronteira).
pub fn spawn_band_load(path: PathBuf, band: usize) -> BandLoadJob {
    let (tx, rx) = std::sync::mpsc::channel::<BandMsg>();
    let job_path = path.clone();
    let spawned = std::thread::Builder::new()
        .name("ta-band".into())
        .spawn(move || {
            let result = load_band(&path, band);
            // Receptor pode ter caído (job substituído): falha de send é
            // o cancelamento — ignorada por design.
            let _ = tx.send(BandMsg::Done { path, band, result });
        });
    if let Err(err) = spawned {
        // Sem thread, `tx` cai com a closure e `rx` desconecta (a UI mostra
        // banda indisponível). Falha de SO não derruba o processo.
        eprintln!("ta-gui: falha ao iniciar a thread de banda: {err}");
    }
    BandLoadJob {
        rx,
        path: job_path,
        band,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_rgba_preserva_pixels() {
        let px = vec![Color32::from_rgb(1, 2, 3), Color32::from_rgb(9, 8, 7)];
        let flat = flatten_rgba(&px);
        assert_eq!(flat, vec![1, 2, 3, 255, 9, 8, 7, 255]);
    }

    #[test]
    fn banda_inexistente_falha_com_erro_legivel() {
        // Fixture pequena versionada (existe no repo, testdata/).
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/fixtures_raster.tif");
        if !path.exists() {
            eprintln!("skipped: fixture de raster ausente (testdata/ vazio)");
            return;
        }
        let err = load_band(&path, 99).expect_err("banda 99 não existe");
        let msg = err.to_string();
        assert!(msg.contains("fora do intervalo"), "mensagem: {msg}");
    }
}
