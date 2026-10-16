//! Smoke test headless da UI completa (Fase 3): constrói o shell inteiro
//! (menu, dock, inspetor, tabela, canvas, status bar) num `egui::Context`
//! sem GPU/display e exercita o fluxo de import local com fixture versionada.
//!
//! O que NÃO cobre (documentado em `benches/map_render.md`): rasterização
//! GPU real, janela nativa, interação de mouse/teclado de SO — isso fica
//! para o checklist de smoke manual.

use eframe::egui;
use ta_gui::app::TerraApp;

/// Um frame da UI inteira; devolve a quantidade de shapes geradas
/// (o `FullOutput` é descartado pela API oficial — deltas de textura não
/// têm backend headless).
fn frame(app: &mut TerraApp) -> usize {
    let ctx = egui::Context::default();
    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| app.show(ui));
    let shapes = std::mem::take(&mut out.shapes);
    out.drop_without_applying_deltas();
    shapes.len()
}

#[test]
fn ui_completa_renderiza_headless_sem_panic() {
    let mut app = TerraApp::new();
    assert!(frame(&mut app) > 0, "shell inteiro deve produzir shapes");

    // Diálogos abrem sem panic (conexão, import, sobre, confirmação).
    app.show_connect = true;
    let _ = frame(&mut app);
    app.show_connect = false;
    app.show_import = true;
    let _ = frame(&mut app);
    app.show_import = false;
    app.show_about = true;
    let _ = frame(&mut app);
    app.show_about = false;
    app.confirm_delete = true;
    let _ = frame(&mut app);
    app.confirm_delete = false;
    assert!(frame(&mut app) > 0);
}

#[test]
fn import_local_de_fixture_vira_camada_no_mapa() {
    // Fixture pequena versionada (testdata/ é gerado; pula se ausente).
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/fixtures_points.csv");
    if !fixture.exists() {
        eprintln!("skipped: testdata/fixtures_points.csv ausente (repo sem fixtures)");
        return;
    }

    let mut app = TerraApp::new();
    app.import_path = fixture.to_string_lossy().to_string();
    app.import_srid = "4674".into();
    app.import_job = Some(ta_gui::import_job::spawn_import(
        fixture.clone(),
        Some(4674),
        "lendo…".into(),
        "normalizando…".into(),
    ));

    // Drena até concluir (thread de fundo → canal → estado → mapa).
    let mut done = false;
    for _ in 0..200 {
        let _ = frame(&mut app);
        if app.import_job.is_none() && !app.state.layers.is_empty() {
            done = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(done, "import local deve concluir e criar camada");
    let layer = &app.state.layers[0];
    assert!(
        !layer.features.is_empty(),
        "CSV de pontos deve trazer features"
    );
    assert_eq!(layer.layer.geom_type, ta_core::layer::GeomType::Point);

    // Um frame após a carga: o canvas deve continuar desenhando.
    assert!(frame(&mut app) > 0);

    // Undo sem edições → false; zoom-to-extent não panica.
    app.zoom_to_extent();
    let _ = frame(&mut app);
    assert!(!app.state.undo());
}

#[test]
fn i18n_pt_br_en_chaves_completas_e_lint() {
    // Chaves completas (contrato "UI 100% via i18n.rs"): o teste canônico
    // vive no módulo; aqui garantimos a troca de idioma em frames reais.
    let mut app = TerraApp::new();
    app.state.i18n.lang = ta_gui::i18n::Lang::En;
    let _ = frame(&mut app);
    app.state.i18n.lang = ta_gui::i18n::Lang::PtBr;
    assert!(frame(&mut app) > 0);
}
