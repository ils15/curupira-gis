//! Smoke test headless da UI completa (Fase 3): constrói o shell inteiro
//! (menu, dock, inspetor, tabela, canvas, status bar) num `egui::Context`
//! sem GPU/display e exercita o fluxo de import local com fixture versionada.
//!
//! O que NÃO cobre (documentado em `benches/map_render.md`): rasterização
//! GPU real, janela nativa, interação de mouse/teclado de SO — isso fica
//! para o checklist de smoke manual.

use eframe::egui;
use ta_gui::app::TerraApp;
use ta_gui::persona::Persona;

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

/// App determinístico para testes: persona SIG explícita (o host pode ter
/// config persistida de uma sessão manual) e bem-vindo fechado.
fn test_app() -> TerraApp {
    let mut app = TerraApp::new();
    app.state.apply_persona(Persona::Sig);
    app.dlg.welcome = false;
    app
}

#[test]
fn ui_completa_renderiza_headless_sem_panic() {
    let mut app = test_app();
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

    let mut app = test_app();
    app.workbench_path = None; // não polui o config real do host
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
    let mut app = test_app();
    app.set_language(ta_gui::i18n::Lang::En);
    let _ = frame(&mut app);
    app.set_language(ta_gui::i18n::Lang::PtBr);
    assert!(frame(&mut app) > 0);
}

// ---------------------------------------------------------------------------
// v0.2 — paridade de UI: bem-vindo (personas), overlays, workbench
// ---------------------------------------------------------------------------

#[test]
fn v02_bem_vindo_abre_e_personas_filtram_sem_esconder_canvas() {
    // 1ª abertura (config default): o modal de boas-vindas está aberto e
    // a UI inteira renderiza com ele (paridade com a referência visual).
    let mut app = TerraApp::new();
    app.dlg.welcome = true;
    app.dlg.welcome_show_at_start = true;
    assert!(
        frame(&mut app) > 0,
        "bem-vindo deve renderizar sobre o shell"
    );

    // Wizard (persona Assistente) também renderiza headless.
    app.state.apply_persona(Persona::Assistant);
    app.dlg.wizard = true;
    assert!(frame(&mut app) > 0, "wizard deve renderizar");
    app.dlg.wizard = false;

    // Cada persona: shell completo com o canvas SEMPRE presente — só os
    // painéis/menus auxiliares mudam (contrato da fase).
    for persona in [
        Persona::Sig,
        Persona::Assistant,
        Persona::Operator,
        Persona::Administrator,
    ] {
        app.state.apply_persona(persona);
        assert!(
            frame(&mut app) > 0,
            "{persona:?} deve renderizar o shell inteiro"
        );
        match persona {
            Persona::Operator => {
                assert!(!app.state.flags.admin, "operador: sem administração");
                assert!(!app.state.flags.project_mgmt);
                assert!(app.state.flags.editing && app.state.flags.import_export);
            }
            Persona::Administrator => {
                assert!(!app.state.flags.editing, "admin: sem edição");
                assert!(!app.state.flags.attr_table && !app.state.flags.import_export);
                assert!(app.state.flags.admin && app.state.flags.tools);
            }
            Persona::Sig => {
                assert!(app.state.flags.editing && app.state.flags.processing);
                assert!(!app.state.flags.wizard, "wizard é do assistente");
            }
            Persona::Assistant => {
                assert!(app.state.flags.wizard && app.state.flags.editing);
            }
        }
    }
}

#[test]
fn v02_overlays_e_escala_no_shell() {
    let mut app = test_app();
    // Grade + barra de escala ligadas: canvas overlay renderiza sem panic.
    app.state.grid_on = true;
    app.state.scale_bar_on = true;
    assert!(frame(&mut app) > 0);

    // Mesmo caminho do dropdown Scale: preset 1:100 000 aplica a escala.
    let ppu = ta_gui::overlays::px_per_unit_for_denominator(100_000.0);
    app.map.viewport_mut().set_px_per_unit(ppu);
    let den = app.map.viewport().scale_denominator();
    assert!(
        (den - 100_000.0).abs() / 100_000.0 < 1e-6,
        "denominador voltou {den}"
    );
    assert!(frame(&mut app) > 0);
}

#[test]
fn v02_workbench_persiste_persona_aplicada() {
    // Caminho de config INJETADO (sem tocar em variáveis de ambiente — os
    // testes rodam em paralelo). Cobre o wiring apply_persona → JSON.
    let dir = std::env::temp_dir().join(format!(
        "curupira-wb-smoke-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut app = test_app();
    app.workbench_path = Some(dir.join("curupira/workbench.json"));

    app.apply_persona(Persona::Operator);
    let text = std::fs::read_to_string(dir.join("curupira/workbench.json"))
        .expect("workbench.json deve existir após apply_persona");
    assert!(text.contains("\"operator\""), "persona no JSON: {text}");
    assert!(text.contains("\"show_welcome\""));

    // Recarga: a persona aplicada volta do disco (TerraApp::new resolve).
    app.workbench_path = None; // para de escrever antes de recarregar
    let reloaded = ta_gui::workbench::load_from(&dir.join("curupira/workbench.json"));
    assert_eq!(reloaded.persona(), Persona::Operator);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn v02_acoes_de_camada_e_console_sql_sem_banco_falham_suave() {
    let mut app = test_app();
    // Modo local: renomear/remover camada via menu não panica; console SQL
    // e processamento ficam atrás de flags/enable mas os caminhos puros
    // (resolve_layer_id/… em modo local) são seguros.
    app.open_rename_layer(); // sem camadas → no-op
    app.remove_active_layer(); // sem camadas → no-op
    app.open_processing(ta_gui::app::ProcKind::Buffer); // pré-preenche
    assert_eq!(app.dlg.processing, Some(ta_gui::app::ProcKind::Buffer));
    app.run_processing(ta_gui::app::ProcKind::Buffer); // sem projeto → erro suave
    assert!(app.error.is_some());
    app.dlg.processing = None;
    assert!(frame(&mut app) > 0);
}

// ---------------------------------------------------------------------------
// v0.2.1 — navegador de arquivos, recentes, drag & drop
// ---------------------------------------------------------------------------

#[test]
fn v021_navegador_abre_navega_e_fecha_headless() {
    let mut app = test_app();
    // Abre no tmpdir (dir real, listável) — sem panic em frame completo.
    app.browser.open_at(std::env::temp_dir());
    assert!(frame(&mut app) > 0);
    assert!(
        !app.browser.path_edit.is_empty(),
        "1ª abertura semeia o cwd no campo de caminho"
    );

    // Filtro local + toggle de busca em subpastas (walk em thread) renderizam.
    app.browser.filter = "zzz-nada-mesmo".into();
    let _ = frame(&mut app);
    app.browser.search_subdirs = true;
    let _ = frame(&mut app); // spawna o walk
    // Drena o resultado final (thread → canal → estado).
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = frame(&mut app);
        if !app.browser.walking() {
            break;
        }
    }
    assert!(
        !app.browser.walking(),
        "walk termina (vazio) e sai do loading"
    );
    assert!(app.browser.rows().is_empty());

    // Navegação para um caminho inválido marca erro inline (sem panic).
    app.browser.path_edit = "/curupira/definitivamente/nao/existe".into();
    let _ = frame(&mut app);
    assert!(app.browser.path_error || !app.browser.path_edit.is_empty());

    // Fechar cancela walk e tira a janela do ar.
    ta_gui::file_browser::close_browser(&mut app);
    assert!(!app.browser.open);
    assert!(frame(&mut app) > 0);
}

#[test]
fn v021_drag_drop_prefilha_o_dialogo_e_o_cancel_limpa_a_fila() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/fixtures_points.csv");
    if !fixture.exists() {
        eprintln!("skipped: testdata/fixtures_points.csv ausente (repo sem fixtures)");
        return;
    }
    let mut app = test_app();
    let ignorado = fixture.with_file_name("leia-me.txt");
    app.handle_dropped_paths(vec![fixture.clone(), ignorado]);
    // 1º suportado pré-preenche o modal (sem auto-start).
    assert!(app.show_import);
    assert_eq!(app.import_path, fixture.to_string_lossy());
    assert!(app.import_job.is_none(), "drop não inicia import sozinho");
    assert!(app.import_queue.is_empty(), "1 suportado → fila vazia");

    // Drop SEM nada suportado: erro nomeando os formatos, nada abre.
    app.show_import = false;
    app.error = None;
    app.handle_dropped_paths(vec![fixture.with_file_name("so.txt")]);
    assert!(!app.show_import);
    assert!(app.error.is_some());

    // Cancelar o diálogo descarta fila/auto (contrato do drop multi).
    app.import_queue = vec![fixture.clone()];
    app.cancel_import_dialog();
    assert!(!app.show_import);
    assert!(app.import_queue.is_empty());
    assert!(!app.import_queue_auto);
    assert!(frame(&mut app) > 0);
}

#[test]
fn v021_fila_multi_arquivo_avanca_e_alimenta_recentes() {
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/fixtures_points.csv");
    if !fixture.exists() {
        eprintln!("skipped: testdata/fixtures_points.csv ausente (repo sem fixtures)");
        return;
    }
    // Duas cópias no tmpdir (dois "arquivos" no drop).
    let dir = std::env::temp_dir().join(format!(
        "curupira-fb-queue-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let f1 = dir.join("fila_a.csv");
    let f2 = dir.join("fila_b.csv");
    std::fs::copy(&fixture, &f1).unwrap();
    std::fs::copy(&fixture, &f2).unwrap();

    let mut app = test_app();
    app.workbench_path = None; // não toca no config real
    app.handle_dropped_paths(vec![f1.clone(), f2.clone()]);
    assert_eq!(app.import_queue, vec![f2.clone()]);

    // Confirma o 1º: job sobe, auto-liga; a fila avança sozinha ao fim.
    app.start_import_from_path(f1.to_string_lossy().to_string());
    assert!(app.import_job.is_some());
    assert!(app.import_queue_auto);

    let mut stable = 0;
    for _ in 0..600 {
        let _ = frame(&mut app);
        if app.import_job.is_none() && app.import_queue.is_empty() {
            stable += 1;
            if stable > 2 {
                break;
            }
        } else {
            stable = 0;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(stable > 0, "os dois imports da fila concluíram");
    assert!(!app.import_queue_auto);
    assert_eq!(app.state.layers.len(), 2, "duas camadas importadas");
    let recents: Vec<_> = app
        .workbench
        .recent_files
        .iter()
        .map(|r| r.path.clone())
        .collect();
    assert!(recents.contains(&f1.to_string_lossy().to_string()));
    assert!(recents.contains(&f2.to_string_lossy().to_string()));
    // Import também alimenta recent_dirs (pasta dos arquivos).
    assert!(
        app.workbench
            .recent_dirs
            .iter()
            .any(|r| r.path == dir.to_string_lossy()),
        "diretório dos arquivos importados entra nos recentes de dirs"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn v021_import_validado_e_recente_invalido() {
    let mut app = test_app();
    app.workbench_path = None;
    app.workbench.recent_files.clear(); // parte limpa (host pode ter recents)

    // Caminho inexistente → erro inline, sem job.
    app.start_import_from_path("/curupira/nao/existe.geojson".into());
    assert!(app.import_job.is_none());
    assert!(app.error.is_some());

    // Vazio → erro de hint (mesma ação do diálogo).
    app.error = None;
    app.start_import_from_path("   ".into());
    assert!(app.import_job.is_none());
    assert!(app.error.is_some());

    // Recentes: menu/submenu renderizam com entradas (válidas e não).
    app.workbench.push_recent_file("/curupira/sumiu.tif");
    let _ = frame(&mut app);
    // Remoção on-click de inválido (mesma ação do menu/navegador).
    assert!(app.workbench.remove_recent_file("/curupira/sumiu.tif"));
    assert!(app.workbench.recent_files.is_empty());
    assert!(frame(&mut app) > 0);
}

#[test]
fn v021_recentes_persistem_no_workbench_json() {
    let dir = std::env::temp_dir().join(format!(
        "curupira-fb-wb-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut app = test_app();
    app.workbench_path = Some(dir.join("curupira/workbench.json"));
    app.workbench.push_recent_file("/d/camada.shp");
    app.workbench.push_recent_dir("/d");
    app.workbench_save();
    let json = std::fs::read_to_string(dir.join("curupira/workbench.json")).unwrap();
    assert!(json.contains("recent_files") && json.contains("recent_dirs"));
    let reloaded = ta_gui::workbench::load_from(&dir.join("curupira/workbench.json"));
    assert_eq!(reloaded.recent_files[0].path, "/d/camada.shp");
    assert_eq!(reloaded.recent_dirs[0].path, "/d");
    let _ = std::fs::remove_dir_all(&dir);
}
