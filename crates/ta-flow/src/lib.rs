//! `ta-flow` — executor do **fluxo de monitoramento**, extraído do `ta-cli`
//! na Fase 5a para que a GUI (Fase 5b) dispare tasks SEM duplicar a lógica de
//! fila/retomada (registro da revisão da Fase 3 no plano, §6/Fase 5).
//!
//! - [`queue`]: a máquina de estados da fila — `create_task` (enfileiramento
//!   idempotente, bloqueadas fora), `run_task` (lote transacional claim →
//!   medidas → `classify_batch` → draft + log + finish no MESMO commit, com
//!   degradação célula a célula quando o lote falha), `cancel_task`
//!   (cooperativo, checado entre lotes) e `retry_task` (repõe `error` em
//!   `pending`; `done` nunca é reprocessado). O progresso sai por
//!   [`queue::RunEvent`] (callback) — nada de stdout aqui: a CLI formata as
//!   MESMAS linhas de antes, a GUI assina os mesmos eventos.
//! - [`area`]: relatório de área consolidado (Decisão 4) — SRS métrico de
//!   `area_settings.srs_id` (default EPSG:5880), NUNCA em graus; totais,
//!   por classe (rascunho) e por célula.
//! - **Preparo do fluxo** (Fase 5b): [`phase`], [`aoi`], [`grid`], [`ruleset`]
//!   e [`measures`] — as ações que a CLI exponha em `phase`/`aoi`/`grid`/
//!   `ruleset`/`measure` e a GUI expõe no painel Monitoramento. A regra de
//!   domínio (unicidade, validação, transação, `max_cells`, `ANALYZE`) mora
//!   aqui: a GUI **não** reimplementa nada, e a CLI vira parse + formatação.
//!
//! Dependências deliberadas (ver `Cargo.toml`): `ta-core`, `ta-db`,
//! `ta-engine` (+ `time` para o instante das medidas) — tipos de conexão via
//! re-export `ta_db::sqlx`. Erros tipados ([`FlowError`]); as mensagens de
//! erro reproduzem EXATAMENTE as da CLI (a Fase 5b consome este crate; a saída
//! visível não muda).

pub mod aoi;
pub mod area;
pub mod error;
pub mod grid;
pub mod measures;
pub mod phase;
pub mod queue;
pub mod ruleset;

pub use error::FlowError;
