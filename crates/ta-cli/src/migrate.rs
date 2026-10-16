//! `ta-cli migrate` — migração de dados para o destino PG16/PostGIS 3.4.
//!
//! Fontes suportadas em `--from-dir`:
//! - **`.shp`** (com sidecars .shx/.dbf/.prj) → `ogr2ogr` com reprojeção
//!   `-t_srs EPSG:4674`, coluna `geom` e tabela `ta_import_<nome>`;
//! - **`.sql`** (dump SQL-text) → aplicado raw multi-statement, com aviso
//!   explícito de que o conteúdo não é validado;
//! - **`.tif`/`.tiff`** → listado e pulado (import de raster é Fase 2).
//!
//! Pós-import cada tabela é validada: `ST_SRID(geom)` tem que ser 4674 e o
//! relatório final é tabela por tabela. O destino tem que ser PostgreSQL ≥ 16
//! (decisão Gate 1: "nunca PG10") e o PostGIS precisa estar disponível.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sqlx::PgPool;
use ta_db::pool::{self, DbConfig};
use ta_db::spatial::{ensure_postgis, is_postgis_ready};

use crate::dsn::mask_password;

/// Argumentos do subcomando `migrate` (clap derive em `main.rs`).
#[derive(Debug, Clone)]
pub struct MigrateArgs {
    /// Diretório com os arquivos de origem (.shp/.tif/.sql).
    pub from_dir: PathBuf,
    /// DSN do destino (`postgres://...`).
    pub to: String,
    /// Só lista o plano, não executa nada.
    pub dry_run: bool,
}

/// Um item do plano de migração, na ordem de execução.
#[derive(Debug, PartialEq)]
enum PlanItem {
    /// Dump SQL-text: aplicar raw.
    Sql { path: PathBuf },
    /// Shapefile: ogr2ogr → tabela `ta_import_<stem>`.
    Shp { path: PathBuf, table: String },
    /// Raster: fora do escopo da Fase 1.
    Raster { path: PathBuf },
}

/// Porta de entrada do subcomando. Erros chegam como texto legível.
pub async fn run(args: MigrateArgs) -> Result<(), String> {
    let items = collect_plan(&args.from_dir)?;
    if items.is_empty() {
        return Err(format!(
            "nenhum arquivo .shp/.tif/.sql encontrado em {}",
            args.from_dir.display()
        ));
    }

    println!(
        "migrate: origem {} → destino {}",
        args.from_dir.display(),
        mask_password(&args.to)
    );
    for (i, item) in items.iter().enumerate() {
        let desc = match item {
            PlanItem::Sql { path } => {
                format!("SQL   {} — aplicar raw multi-statement", path.display())
            }
            PlanItem::Shp { path, table } => format!(
                "SHP   {} — ogr2ogr → tabela {table} (reprojetar p/ EPSG:4674)",
                path.display()
            ),
            PlanItem::Raster { path } => {
                format!("TIF   {} — PULAR (raster: Fase 2)", path.display())
            }
        };
        println!("  [{:>2}/{}] {desc}", i + 1, items.len());
    }

    if args.dry_run {
        println!("dry-run: nada foi executado.");
        return Ok(());
    }

    let cfg = DbConfig {
        url: args.to.clone(),
        max_connections: 4,
    };
    let pool = pool::connect(&cfg)
        .await
        .map_err(|e| format!("conexão ao destino falhou: {e}"))?;
    check_pg_version(&pool).await?;

    let has_shp = items.iter().any(|i| matches!(i, PlanItem::Shp { .. }));
    if has_shp && !is_postgis_ready(&pool).await.map_err(|e| e.to_string())? {
        ensure_postgis(&pool).await.map_err(|e| e.to_string())?;
    }

    let mut report: Vec<String> = Vec::new();
    for item in &items {
        match item {
            PlanItem::Sql { path } => {
                let sql = fs::read_to_string(path)
                    .map_err(|e| format!("lendo {}: {e}", path.display()))?;
                println!(
                    "AVISO: aplicando SQL bruto sem validação de conteúdo: {}",
                    path.display()
                );
                // AssertSqlSafe: conteúdo vindo do operador por decisão dele
                // (dump SQL-text); executado via simple protocol, sem binds.
                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                    .execute(&pool)
                    .await
                    .map_err(|e| format!("aplicando {}: {e}", path.display()))?;
                report.push(format!("SQL   {} — aplicado", path.display()));
            }
            PlanItem::Shp { path, table } => {
                let gdal_dsn = dsn_to_gdal_pg(&args.to);
                println!("importando {} → {table} …", path.display());
                run_ogr2ogr(path, &gdal_dsn, table)?;
                let (srid, rows) = validate_imported_table(&pool, table).await?;
                match srid {
                    Some(4674) => {
                        report.push(format!("SHP   {table} — {rows} features, SRID 4674 ✓"))
                    }
                    Some(other) => {
                        return Err(format!(
                            "tabela {table}: SRID {other} ≠ 4674 após reprojeção; \
                             verifique o .prj da origem e o suporte a -t_srs no GDAL"
                        ));
                    }
                    None => report.push(format!("SHP   {table} — {rows} features (tabela vazia)")),
                }
            }
            PlanItem::Raster { path } => {
                report.push(format!("TIF   {} — pulado (Fase 2)", path.display()));
            }
        }
    }

    println!("relatório final:");
    for line in &report {
        println!("  {line}");
    }
    Ok(())
}

/// Varre o diretório e monta o plano (ordem determinística por nome).
fn collect_plan(dir: &Path) -> Result<Vec<PlanItem>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("lendo {}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();

    let mut items = Vec::new();
    for path in files {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "sql" => items.push(PlanItem::Sql { path }),
            "shp" => items.push(PlanItem::Shp {
                table: sanitize_table_name(&stem_of(&path)),
                path,
            }),
            "tif" | "tiff" => items.push(PlanItem::Raster { path }),
            // sidecars do shapefile e desconhecidos: fora do plano
            _ => {}
        }
    }
    Ok(items)
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_owned()
}

/// `ta_import_<stem>` sanitizado para identificador PostgreSQL: minúsculas,
/// `[a-z0-9_]`, resto vira `_`; cortado em 63 bytes (limite do PG).
/// O prefixo fixo garante que a tabela comece com letra.
fn sanitize_table_name(stem: &str) -> String {
    const PG_IDENT_MAX: usize = 63;
    const PREFIX: &str = "ta_import_";
    let mut out = String::from(PREFIX);
    for ch in stem.chars() {
        if out.len() >= PG_IDENT_MAX {
            break;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    out
}

/// Verdadeiro identificador seguro (defesa em profundidade antes de interpolar
/// o nome entre aspas no SQL de validação — identificadores não são bindáveis).
fn is_safe_identifier(table: &str) -> bool {
    !table.is_empty()
        && table.len() <= 63
        && table.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && table
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Converte o DSN para o formato do driver PostgreSQL do GDAL.
///
/// - `postgres://user:pass@host:port/db` → `PG:host=… port=… dbname=… user=… password=…`
/// - `postgres://host/db` (sem userinfo) → `PG:host=… dbname=…`
/// - qualquer outra forma (keyword=value) → `PG:<dsn>` intocado.
///
/// Limitação documentada: userinfo com `@`/`:` percent-codificados não é
/// decodificado aqui (encode percentual não é interpretado).
fn dsn_to_gdal_pg(dsn: &str) -> String {
    let rest = match dsn
        .strip_prefix("postgresql://")
        .or_else(|| dsn.strip_prefix("postgres://"))
    {
        Some(rest) => rest,
        None => return format!("PG:{dsn}"),
    };
    let (authority, dbname) = match rest.split_once('/') {
        Some((a, d)) => (a, d),
        None => (rest, ""),
    };
    let (user, pass, hostport) = match authority.rsplit_once('@') {
        Some((userinfo, hp)) => match userinfo.split_once(':') {
            Some((u, p)) => (u, p, hp),
            None => (userinfo, "", hp),
        },
        None => ("", "", authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() => (h, p),
        _ => (hostport, ""),
    };

    let mut out = String::from("PG:");
    if !host.is_empty() {
        out.push_str(&format!("host={host} "));
    }
    if !port.is_empty() {
        out.push_str(&format!("port={port} "));
    }
    if !dbname.is_empty() {
        out.push_str(&format!("dbname={dbname} "));
    }
    if !user.is_empty() {
        out.push_str(&format!("user={user} "));
    }
    if !pass.is_empty() {
        out.push_str(&format!("password={pass} "));
    }
    out.trim_end().to_owned()
}

/// Executa ogr2ogr com as flags da decisão Gate 1.
fn run_ogr2ogr(shp: &Path, gdal_dsn: &str, table: &str) -> Result<(), String> {
    if !is_safe_identifier(table) {
        return Err(format!("nome de tabela gerado é inseguro: {table:?}"));
    }
    println!(
        "  comando: ogr2ogr -f PostgreSQL PG:{} {} -t_srs EPSG:4674 -lco GEOMETRY_NAME=geom -nln {table}",
        // não imprimir credenciais
        gdal_dsn
            .split_whitespace()
            .filter(|kv| !kv.starts_with("password="))
            .collect::<Vec<_>>()
            .join(" ")
            .trim_start_matches("PG:"),
        shp.display()
    );
    let out = Command::new("ogr2ogr")
        .arg("-f")
        .arg("PostgreSQL")
        .arg(gdal_dsn)
        .arg(shp)
        .arg("-t_srs")
        .arg("EPSG:4674")
        .arg("-lco")
        .arg("GEOMETRY_NAME=geom")
        .arg("-nln")
        .arg(table)
        .output()
        .map_err(|e| {
            format!("falha ao executar ogr2ogr ({e}); instale GDAL/ogr2ogr >= 3.4 no PATH")
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "ogr2ogr falhou para {} → {table} (a tabela é criada sem -overwrite; \
             remova-a manualmente se quiser repetir):\n{}",
            shp.display(),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Validação pós-import: SRID da coluna `geom` (DISTINCT) + contagem.
/// Retorna (srid, rows); `None` = tabela vazia.
async fn validate_imported_table(pool: &PgPool, table: &str) -> Result<(Option<i32>, i64), String> {
    if !is_safe_identifier(table) {
        return Err(format!("nome de tabela gerado é inseguro: {table:?}"));
    }
    // Interpolação de identificador é inevitável (SQL não tem bind de
    // identificadores); é segura porque `table` passa por `is_safe_identifier`
    // (^[a-z_][a-z0-9_]*$, <=63 bytes) e vem do prefixo fixo `ta_import_` +
    // nome sanitizado — nunca de dados do usuário.
    let srids: Vec<i32> = sqlx::query_scalar::<_, i32>(sqlx::AssertSqlSafe(format!(
        r#"SELECT DISTINCT ST_SRID(geom) FROM "{table}" WHERE geom IS NOT NULL"#
    )))
    .fetch_all(pool)
    .await
    .map_err(|e| {
        format!(
            "validando SRID de {table}: {e} — a tabela tem coluna `geom`? \
                 (ogr2ogr precisa rodar com -lco GEOMETRY_NAME=geom)"
        )
    })?;
    let srid = match srids.len() {
        0 => None,
        1 => Some(srids[0]),
        _ => {
            return Err(format!(
                "tabela {table}: SRIDs misturados {srids:?} — reprojeção incompleta"
            ));
        }
    };
    let rows: i64 = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        r#"SELECT count(*) FROM "{table}""#
    )))
    .fetch_one(pool)
    .await
    .map_err(|e| format!("contando {table}: {e}"))?;
    Ok((srid, rows))
}

/// Recusa destino < PG16 (decisão Gate 1: "nunca PG10"; alvo é PG16).
async fn check_pg_version(pool: &PgPool) -> Result<(), String> {
    let num: i64 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int8")
        .fetch_one(pool)
        .await
        .map_err(|e| format!("lendo server_version_num: {e}"))?;
    if num < 160_000 {
        return Err(format!(
            "destino é PostgreSQL {num} (< 160000); o migrate exige PG >= 16 (nunca PG10)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_produz_identificador_seguro() {
        assert_eq!(
            sanitize_table_name("Imóveis-Rurais 2024"),
            "ta_import_im_veis_rurais_2024"
        );
        assert_eq!(sanitize_table_name("ABC"), "ta_import_abc");
        assert!(is_safe_identifier(&sanitize_table_name("x.y z")));
        assert_eq!(sanitize_table_name("x.y z"), "ta_import_x_y_z");
    }

    #[test]
    fn sanitize_respeita_limite_de_63_bytes() {
        let long = "a".repeat(200);
        let t = sanitize_table_name(&long);
        assert!(t.len() <= 63);
        assert!(is_safe_identifier(&t));
    }

    #[test]
    fn identificador_inseguro_e_rejeitado() {
        assert!(!is_safe_identifier(""));
        assert!(!is_safe_identifier("1abc"));
        assert!(!is_safe_identifier("ab;drop table x"));
        assert!(!is_safe_identifier("Abc"));
        assert!(is_safe_identifier("ta_import_a1_"));
    }

    #[test]
    fn dsn_uri_vira_keyword_form() {
        assert_eq!(
            dsn_to_gdal_pg("postgres://ta:segredo@127.0.0.1:55432/ta"),
            "PG:host=127.0.0.1 port=55432 dbname=ta user=ta password=segredo"
        );
        assert_eq!(
            dsn_to_gdal_pg("postgresql://localhost/meubanco"),
            "PG:host=localhost dbname=meubanco"
        );
        // keyword=value passa intocado com prefixo PG:
        assert_eq!(dsn_to_gdal_pg("host=x dbname=y"), "PG:host=x dbname=y");
    }
}
