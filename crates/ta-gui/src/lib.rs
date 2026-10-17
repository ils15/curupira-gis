//! `ta-gui` — aplicativo desktop do terraamazon-cleanroom-rust.
//!
//! Backend de render do MVP: **egui/eframe**. O canvas de mapa vive atrás da
//! trait [`map_view::MapView`], o que permite trocar o backend (ex.:
//! Tauri+MapLibre na Fase 3b, condicional ao bench) sem reescrever o app.
//!
//! Estrutura (Fase 3):
//! - [`viewport`]: matemática pura de tela↔mundo, LOD e hit-test (testável
//!   headless, sem GPU/display);
//! - [`map_view`]: trait [`map_view::MapView`] + [`map_view::MapViewEgui`]
//!   (pan/zoom/render vetorial+raster/pick/zoom_to);
//! - [`draw`]: rotinas egui de desenho usadas pelo `MapViewEgui`;
//! - [`raster_view`]: banda GeoTIFF → imagem egui (downsample via GDAL);
//! - [`i18n`]: **todos** os rótulos da UI passam por aqui (pt-BR/en);
//! - [`db_worker`]: thread de banco (tokio runtime dedicado + mpsc) — a UI
//!   nunca bloqueia em I/O;
//! - [`app_state`]: modelo de dados da sessão (camadas locais/banco,
//!   seleção, undo/redo de atributos);
//! - [`panels`]/[`app`]: dock de camadas, tabela atributiva, inspetor,
//!   status bar, diálogos e atalhos de teclado.

pub mod app;
pub mod app_state;
pub mod attr_table;
pub mod command_registry;
pub mod db_worker;
pub mod dialogs;
pub mod draw;
pub mod export_job;
pub mod file_browser;
pub mod i18n;
pub mod import_job;
pub mod map_view;
pub mod menus;
pub mod overlays;
pub mod palette;
pub mod panels;
pub mod persona;
pub mod raster_view;
pub mod status_bar;
pub mod toolbox;
pub mod viewport;
pub mod welcome;
pub mod work_dialogs;
pub mod workbench;
