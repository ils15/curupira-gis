//! Subcomando `area` (Fase 5): o relatório de área consolidado.
//!
//! Camada fina: a orquestração (grade → SRS efetivo → agregações) é
//! [`ta_flow::area::report`]; aqui é só formatação — tabela legível em
//! **hectares** (ha) ou JSON (`--json`) para consumo programático. O SRS
//! usado aparece SEMPRE na saída (a área sem o SRS não é verificável —
//! Decisão 4: mesma geometria, SRSs diferentes, números diferentes).
//!
//! Formato do `--json` (o contrato é o próprio `AreaReport` de `ta-core`,
//! serializado — mesmo formato para CLI e GUI; áreas sempre em m² no SRS
//! informado, conversão p/ ha é só exibição):
//!
//! ```json
//! {
//!   "cell_layer_id": "uuid", "cell_layer_name": "grade",
//!   "srs": 5880, "srs_units": "metros",
//!   "summary": { "total_m2": 0.0, "blocked_m2": 0.0, "available_m2": 0.0,
//!                "cells_total": 0, "cells_blocked": 0 },
//!   "by_class": [ { "class_name": "floresta", "cells": 1, "area_m2": 0.0 } ]
//! }
//! ```

use ta_flow::area;

/// Fator m² → hectare (10 000 m² = 1 ha).
const M2_PER_HA: f64 = 10_000.0;

/// `area report --cell-layer <id> [--draft <nome>] [--srs <epsg>] [--json]`.
pub(crate) async fn area_report(
    dsn: Option<&str>,
    cell_layer: &str,
    draft: Option<&str>,
    srs: Option<i32>,
    as_json: bool,
) -> Result<(), String> {
    let pool = crate::open_pool(dsn).await?;
    let report = area::report(&pool, cell_layer, draft, srs)
        .await
        .map_err(|e| e.to_string())?;

    if as_json {
        // O tipo `AreaReport` (ta-core) já é o contrato de serialização —
        // mesmo formato para CLI e GUI; floats com round-trip byte-a-byte
        // (feature `float_roundtrip` do serde_json no workspace).
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        let s = report.summary;
        println!(
            "área da grade: {} ({})",
            report.cell_layer_name, report.cell_layer_id
        );
        println!("srs de cálculo: EPSG:{} ({})", report.srs, report.srs_units);
        println!(
            "total: {:.2} ha\t({} células)",
            s.total_m2 / M2_PER_HA,
            s.cells_total
        );
        println!(
            "bloqueada: {:.2} ha\t({} células)",
            s.blocked_m2 / M2_PER_HA,
            s.cells_blocked
        );
        println!("disponível: {:.2} ha", s.available_m2 / M2_PER_HA);
        if let Some(name) = draft {
            if report.by_class.is_empty() {
                println!("(nenhuma célula classificada no rascunho `{name}`)");
            } else {
                println!("por classe (rascunho `{name}`):");
                for class in &report.by_class {
                    println!(
                        "  {}\t{} células\t{:.2} ha",
                        class.class_name,
                        class.cells,
                        class.area_m2 / M2_PER_HA
                    );
                }
            }
        }
    }
    pool.close().await;
    Ok(())
}
