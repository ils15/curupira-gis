//! Binário desktop **Curupira GIS** (crate interno `ta-gui`, eframe).
//! Fase 3: shell completo (dock de camadas, canvas MapViewEgui real, tabela
//! atributiva, import, banco). Fase 4: bin renomeado de `ta-gui` para
//! `curupira-gis` (marca pública; crate mantém o prefixo `ta-`).

use eframe::egui;
use std::path::{Path, PathBuf};

use ta_gui::app::TerraApp;
use ta_gui::i18n::{Key, Lang, tr};

/// Diretório de dados bundled (`proj`/`gdal`) na 1ª base que o possua:
///   1. `<dir-do-exe>/share` — zip Windows (instalador .iss instala em
///      `<app>\share`) e tar.gz Linux (`share` ao lado do binário);
///   2. `<dir-do-exe>/../Resources/share` — padrão de bundle macOS (.app):
///      código em Contents/MacOS, dados em Contents/Resources. O codesign
///      trata TODO arquivo sob MacOS/ como código (run #5: dado do PROJ em
///      MacOS/share derrubou o sign do bundle), logo dados NUNCA ficam lá.
fn bundled_data_dir(exe_dir: &Path, name: &str) -> Option<PathBuf> {
    [
        exe_dir.join("share"),
        exe_dir.join("..").join("Resources").join("share"),
    ]
    .into_iter()
    .map(|share| share.join(name))
    .find(|dir| dir.is_dir())
}

/// Aponta `PROJ_LIB`/`GDAL_DATA` para dados embarcados ao lado do executável
/// (ou em `../Resources/share`, no bundle macOS), se existirem. No-op quando
/// os diretórios não existem (Linux com GDAL do sistema) ou já definidos pelo
/// usuário — variáveis de ambiente têm prioridade.
fn use_bundled_gdal_data() {
    let exe_dir = match std::env::current_exe() {
        Ok(dir) => dir,
        // Sem caminho de exe (host exótico): segue sem dados bundled.
        Err(_) => return,
    };
    let proj = bundled_data_dir(&exe_dir, "proj");
    let gdal = bundled_data_dir(&exe_dir, "gdal");
    // SAFETY: `set_var` é unsafe na edition 2024 por sofrer corrida com
    // threads que leem o ambiente. Aqui roda no início de `main`, na
    // única thread do processo, antes de eframe/winit/tokio criarem
    // qualquer thread — nenhum leitor concorrente existe neste ponto.
    unsafe {
        if let Some(proj) = proj.filter(|_| std::env::var_os("PROJ_LIB").is_none()) {
            std::env::set_var("PROJ_LIB", proj);
        }
        if let Some(gdal) = gdal.filter(|_| std::env::var_os("GDAL_DATA").is_none()) {
            std::env::set_var("GDAL_DATA", gdal);
        }
    }
}

fn main() -> eframe::Result<()> {
    use_bundled_gdal_data();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title(tr(Lang::default(), Key::AppTitle)),
        ..Default::default()
    };
    eframe::run_native(
        tr(Lang::default(), Key::AppTitle),
        options,
        Box::new(|_cc| Ok(Box::new(TerraApp::new()))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolver de dados bundled: 1º `<exe_dir>/share` (Windows/Linux —
    /// instalador .iss em `{app}\share` e tar.gz ao lado do binário) e, só se
    /// ausente, `<exe_dir>/../Resources/share` (bundle macOS). Regressão da
    /// run #5: os dados do PROJ/GDAL migraram de MacOS/ p/ Resources/ no .app.
    #[test]
    fn resolver_prefere_share_e_cai_para_resources() {
        let root = std::env::temp_dir().join(format!(
            "curupira-bundled-data-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let exe_dir = root.join("CurupiraGIS.app/Contents/MacOS");

        // Nenhum layout presente → None (GDAL do sistema, sem override).
        assert_eq!(bundled_data_dir(&exe_dir, "proj"), None);

        // Layout Windows/Linux: <exe_dir>/share/proj vence.
        std::fs::create_dir_all(exe_dir.join("share/proj")).unwrap();
        assert_eq!(
            bundled_data_dir(&exe_dir, "proj").map(|p| p.canonicalize().unwrap()),
            Some(exe_dir.join("share/proj").canonicalize().unwrap())
        );

        // Bundle macOS: nome ausente em share/ cai p/ ../Resources/share.
        std::fs::create_dir_all(exe_dir.join("../Resources/share/gdal")).unwrap();
        assert_eq!(
            bundled_data_dir(&exe_dir, "gdal").map(|p| p.canonicalize().unwrap()),
            Some(
                exe_dir
                    .join("../Resources/share/gdal")
                    .canonicalize()
                    .unwrap()
            )
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
