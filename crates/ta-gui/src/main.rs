//! Binário desktop **Curupira GIS** (crate interno `ta-gui`, eframe).
//! Fase 3: shell completo (dock de camadas, canvas MapViewEgui real, tabela
//! atributiva, import, banco). Fase 4: bin renomeado de `ta-gui` para
//! `curupira-gis` (marca pública; crate mantém o prefixo `ta-`).

use eframe::egui;

use ta_gui::app::TerraApp;
use ta_gui::i18n::{Key, Lang, tr};

/// Aponta `PROJ_LIB`/`GDAL_DATA` para dados embarcados ao lado do executável,
/// se existirem (layout do zip de release Windows: `share/proj`, `share/gdal`).
/// No-op quando os diretórios não existem (Linux com GDAL do sistema) ou já
/// definidos pelo usuário — variáveis de ambiente têm prioridade.
fn use_bundled_gdal_data() {
    let exe_dir = match std::env::current_exe() {
        Ok(dir) => dir,
        // Sem caminho de exe (host exótico): segue sem dados bundled.
        Err(_) => return,
    };
    let proj = exe_dir.join("share").join("proj");
    let gdal = exe_dir.join("share").join("gdal");
    if (proj.is_dir() && std::env::var_os("PROJ_LIB").is_none())
        || (gdal.is_dir() && std::env::var_os("GDAL_DATA").is_none())
    {
        // SAFETY: `set_var` é unsafe na edition 2024 por sofrer corrida com
        // threads que leem o ambiente. Aqui roda no início de `main`, na
        // única thread do processo, antes de eframe/winit/tokio criarem
        // qualquer thread — nenhum leitor concorrente existe neste ponto.
        unsafe {
            if proj.is_dir() && std::env::var_os("PROJ_LIB").is_none() {
                std::env::set_var("PROJ_LIB", &proj);
            }
            if gdal.is_dir() && std::env::var_os("GDAL_DATA").is_none() {
                std::env::set_var("GDAL_DATA", &gdal);
            }
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
