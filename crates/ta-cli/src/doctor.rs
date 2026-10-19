//! `ta-cli doctor` — diagnóstico do ambiente.
//!
//! Checagens (na ordem): conexão com o PostgreSQL, versão do servidor,
//! PostGIS (+ suporte MVT), migrações aplicadas, GDAL em runtime e permissão
//! de escrita no diretório de trabalho. Cada falha traz uma **remediação
//! explícita** ("o que fazer") — nunca stacktrace cru.
//!
//! Exit code: `0` quando nada falha (avisos não reprovam); `1` se alguma
//! checagem falhar. Banco fora do ar é um caso previsto: o relatório aponta a
//! causa e o remédio, sem panic.
//!
//! Este módulo conecta com [`ta_db::pool::connect`] e **não** aplica
//! migrações de propósito — senão a checagem "migrações aplicadas" seria
//! sempre verde. Quem aplica é `ta-cli migrate`.

use ta_db::pool::{self, DbConfig};
use ta_db::spatial;

use crate::dsn::mask_password;

/// Tempo máximo esperando o banco responder no diagnóstico. Curto de
/// propósito: com o banco fora, o usuário quer saber agora, não em 30 s.
const DIAGNOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Resultado de uma checagem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Tudo certo.
    Ok,
    /// Funciona, mas há algo a observar (não reprova).
    Warn,
    /// Bloqueia o uso — reprova.
    Fail,
}

/// Uma checagem do diagnóstico.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    /// O que o usuário deve fazer quando não está `Ok`.
    pub remedy: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Ok,
            detail: detail.into(),
            remedy: None,
        }
    }

    fn warn(name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail: detail.into(),
            remedy: Some(remedy.into()),
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail: detail.into(),
            remedy: Some(remedy.into()),
        }
    }
}

/// Relatório completo do ambiente.
#[derive(Debug, Clone)]
pub struct Report {
    /// DSN mascarado (senha nunca aparece).
    pub dsn: String,
    pub checks: Vec<Check>,
}

impl Report {
    /// `0` = nenhuma falha; `1` = ao menos uma falha.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        u8::from(self.checks.iter().any(|c| c.status == Status::Fail))
    }

    fn count(&self, status: Status) -> usize {
        self.checks.iter().filter(|c| c.status == status).count()
    }

    /// Relatório legível para o terminal.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from("ta-cli doctor — diagnóstico do ambiente\n");
        out.push_str(&format!("dsn: {}\n\n", self.dsn));
        for check in &self.checks {
            let mark = match check.status {
                Status::Ok => "✓",
                Status::Warn => "!",
                Status::Fail => "✗",
            };
            out.push_str(&format!("  {mark} {:<10} {}\n", check.name, check.detail));
            if let Some(remedy) = &check.remedy {
                out.push_str(&format!("      → {remedy}\n"));
            }
        }
        out.push_str(&format!(
            "\n{} ok, {} aviso(s), {} falha(s)\n",
            self.count(Status::Ok),
            self.count(Status::Warn),
            self.count(Status::Fail),
        ));
        if self.exit_code() == 0 {
            out.push_str("ambiente pronto.\n");
        } else {
            out.push_str("ambiente NÃO está pronto — corrija as falhas acima.\n");
        }
        out
    }
}

/// Diagnostica o ambiente para o DSN resolvido (flag → env → default).
pub async fn diagnose(dsn: Option<&str>) -> Report {
    let url = crate::resolve_dsn(dsn);
    let mut checks = Vec::new();

    match pool::connect_with_timeout(
        &DbConfig {
            url: url.clone(),
            max_connections: 1,
        },
        DIAGNOSE_TIMEOUT,
    )
    .await
    {
        Ok(db) => {
            checks.extend(db_checks(&db).await);
            db.close().await;
        }
        Err(e) => checks.push(Check::fail(
            "conexão",
            format!("não foi possível conectar ({e})"),
            format!(
                "suba o banco e confira o DSN: `bash scripts/dev-up.sh` (com docker) ou \
                 `scripts/test-pg.sh --no-tests` (sem docker). DSN em uso: {}",
                mask_password(&url)
            ),
        )),
    }

    checks.push(gdal_check());
    checks.push(write_check());

    Report {
        dsn: mask_password(&url),
        checks,
    }
}

/// Checagens que exigem o banco conectado.
async fn db_checks(db: &sqlx::PgPool) -> Vec<Check> {
    let mut checks = Vec::new();

    // Versão do servidor (PG >= 16 é requisito do projeto).
    match sqlx::query_as::<_, (String, i64)>(
        "SELECT current_setting('server_version'), current_setting('server_version_num')::int8",
    )
    .fetch_one(db)
    .await
    {
        Ok((version, num)) if num >= 160_000 => {
            checks.push(Check::ok("postgres", format!("PostgreSQL {version}")))
        }
        Ok((version, num)) => checks.push(Check::fail(
            "postgres",
            format!("servidor é PostgreSQL {version} ({num} < 160000)"),
            "use PostgreSQL 16+: `docker compose up -d postgis` sobe postgis/postgis:16-3.4",
        )),
        Err(e) => checks.push(Check::fail(
            "postgres",
            format!("não foi possível ler a versão do servidor ({e})"),
            "verifique se o banco é PostgreSQL (o clean-room não suporta outro motor)",
        )),
    }

    // PostGIS instalado + reporte da versão (o suporte MVT tem checagem
    // própria abaixo — o version string completo é longo demais para a linha).
    match sqlx::query_scalar::<_, String>("SELECT postgis_full_version()")
        .fetch_one(db)
        .await
    {
        Ok(full) => checks.push(Check::ok("postgis", postgis_short(&full))),
        Err(_) => checks.push(Check::fail(
            "postgis",
            "extensão postgis ausente neste banco",
            "instale/ative o PostGIS 3.4 (imagem `postgis/postgis:16-3.4` já traz; no \
             PostgreSQL nativo: `CREATE EXTENSION postgis;` como superusuário)",
        )),
    }

    // Tiles MVT: dependem de ST_AsMVT com protobuf-c — builds conda-forge
    // normalmente não têm (o mapa funciona; só os tiles vetoriais ficam off).
    match spatial::mvt_available(db).await {
        Ok(true) => checks.push(Check::ok(
            "mvt",
            "ST_AsMVT disponível (protobuf-c presente)",
        )),
        Ok(false) => checks.push(Check::warn(
            "mvt",
            "ST_AsMVT indisponível (PostGIS sem protobuf-c) — tiles vetoriais ficam off",
            "use a imagem `postgis/postgis:16-3.4` (tem protobuf-c); camadas vetoriais \
             seguem funcionando sem tiles",
        )),
        Err(e) => checks.push(Check::warn(
            "mvt",
            format!("não foi possível testar ST_AsMVT ({e})"),
            "afeta só a geração de tiles, não as camadas vetoriais",
        )),
    }

    // Migrações: comparadas com os arquivos versionados em ta-db/migrations.
    let local = spatial::local_migration_versions()
        .await
        .unwrap_or_default();
    let applied = spatial::applied_migration_versions(db)
        .await
        .unwrap_or_default();
    let pending: Vec<i64> = local
        .iter()
        .copied()
        .filter(|v| !applied.contains(v))
        .collect();
    if local.is_empty() {
        checks.push(Check::warn(
            "migrações",
            "não consegui listar as migrações locais (`migrations/` inacessível)",
            "o caminho das migrações é embutido no build; rode o binário do repositório \
             (ou recompile) para o diagnóstico completo",
        ));
    } else if !pending.is_empty() {
        checks.push(Check::fail(
            "migrações",
            format!(
                "faltam {} migração(ões) ({}) — aplicadas {}",
                pending.len(),
                pending
                    .iter()
                    .map(|v| format!("{v:04}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                applied.len()
            ),
            "aplique o schema: `ta-cli migrate --to <dsn>` (idempotente)",
        ));
    } else if applied.len() > local.len() {
        checks.push(Check::warn(
            "migrações",
            format!(
                "o banco tem mais migrações ({}) que este binário ({})",
                applied.len(),
                local.len()
            ),
            "atualize/recompile o binário (ele pode não entender o schema novo)",
        ));
    } else {
        checks.push(Check::ok(
            "migrações",
            format!(
                "{}/{} aplicadas",
                applied.len().min(local.len()),
                local.len()
            ),
        ));
    }

    checks
}

/// Versão curta do PostGIS a partir de `postgis_full_version()`
/// (`POSTGIS="3.4.3 37edef7" [EXTENSION] …` → `PostGIS 3.4.3`). Fallback:
/// primeiros caracteres do texto original.
fn postgis_short(full: &str) -> String {
    let version = full
        .split('"')
        .nth(1)
        .and_then(|inner| inner.split_whitespace().next())
        .filter(|v| !v.is_empty());
    match version {
        Some(v) => format!("PostGIS {v}"),
        None => format!(
            "PostGIS ({})",
            full.trim().chars().take(80).collect::<String>()
        ),
    }
}

/// GDAL: com a feature `gdal` o binário linka libgdal dinamicamente, então
/// pedir a versão em runtime prova que a biblioteca está carregável.
fn gdal_check() -> Check {
    #[cfg(feature = "gdal")]
    {
        let summary = ta_gis::gdal_version_summary();
        if summary.trim().is_empty() {
            Check::warn(
                "gdal",
                "libgdal respondeu vazio (GDAL_VERSION_INFO indisponível)",
                "verifique a instalação do GDAL do sistema (`apt install libgdal-dev`)",
            )
        } else {
            Check::ok("gdal", summary)
        }
    }
    #[cfg(not(feature = "gdal"))]
    {
        Check::warn(
            "gdal",
            "binário compilado sem a feature `gdal` (SHP/GPKG/GeoTIFF indisponíveis)",
            "compile com a feature padrão: `cargo build -p ta-cli` (gdal = link dinâmico)",
        )
    }
}

/// Escrita: import/export e o seed escrevem arquivos no diretório do usuário;
/// checamos o diretório de trabalho (onde o usuário está) e o temporário.
fn write_check() -> Check {
    fn probe(dir: &std::path::Path) -> std::io::Result<()> {
        let file = dir.join(format!(".ta-doctor-{}", std::process::id()));
        std::fs::write(&file, b"ok")?;
        std::fs::remove_file(&file)
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match probe(&cwd) {
        Ok(()) => Check::ok(
            "escrita",
            format!("diretório atual gravável ({})", cwd.display()),
        ),
        Err(e) => {
            let tmp = std::env::temp_dir();
            match probe(&tmp) {
                Ok(()) => Check::warn(
                    "escrita",
                    format!("diretório atual NÃO gravável ({}): {e}", cwd.display()),
                    format!(
                        "rode de um diretório com permissão de escrita (o temporário \
                         {} funciona para export/derivados)",
                        tmp.display()
                    ),
                ),
                Err(e2) => Check::fail(
                    "escrita",
                    format!(
                        "sem permissão de escrita em {} ({e}) nem em {} ({e2})",
                        cwd.display(),
                        tmp.display()
                    ),
                    "ajuste as permissões (`chmod u+w`) ou rode em um diretório seu",
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &'static str, status: Status) -> Check {
        Check {
            name,
            status,
            detail: String::new(),
            remedy: None,
        }
    }

    #[test]
    fn versao_curta_do_postgis() {
        assert_eq!(
            postgis_short(r#"POSTGIS="3.4.3 37edef7" [EXTENSION] PGSQL="160""#),
            "PostGIS 3.4.3"
        );
        assert!(postgis_short("").starts_with("PostGIS ("));
    }

    #[test]
    fn exit_code_e_zero_com_ok_e_aviso() {
        let report = Report {
            dsn: "postgres://ta:***@127.0.0.1:5432/ta".into(),
            checks: vec![check("a", Status::Ok), check("b", Status::Warn)],
        };
        assert_eq!(report.exit_code(), 0);
        assert!(report.render().contains("ambiente pronto."));
    }

    #[test]
    fn exit_code_e_1_com_qualquer_falha() {
        let report = Report {
            dsn: "postgres://ta:***@127.0.0.1:5432/ta".into(),
            checks: vec![check("a", Status::Ok), check("b", Status::Fail)],
        };
        assert_eq!(report.exit_code(), 1);
        assert!(report.render().contains("NÃO está pronto"));
    }

    /// Banco fora do ar = relatório com falha + remédio, sem panic.
    #[tokio::test]
    async fn banco_fora_do_ar_falha_com_mensagem_acionavel() {
        // Porta inválida (> 65535): sqlx falha no parse do DSN, rápido e sem
        // servidor — exatamente o "banco fora do ar" em tempo de execução.
        let report = diagnose(Some("postgres://ta:segredo@127.0.0.1:99999/ta")).await;
        assert_eq!(report.exit_code(), 1, "deve reprovar");
        let conn = report
            .checks
            .iter()
            .find(|c| c.name == "conexão")
            .expect("checagem de conexão existe");
        assert_eq!(conn.status, Status::Fail);
        let remedy = conn.remedy.as_deref().unwrap_or_default();
        assert!(remedy.contains("dev-up.sh"), "remédio acionável: {remedy}");
        assert!(
            !report.render().contains("segredo"),
            "a senha vazou no relatório"
        );
    }

    /// GDAL é independente do banco: roda mesmo com o banco fora do ar.
    #[tokio::test]
    async fn checagens_locais_rodam_sem_banco() {
        let report = diagnose(Some("postgres://ta:ta@127.0.0.1:99999/ta")).await;
        let names: Vec<&str> = report.checks.iter().map(|c| c.name).collect();
        assert!(names.contains(&"gdal"), "checagens: {names:?}");
        assert!(names.contains(&"escrita"), "checagens: {names:?}");
        let escrita = report
            .checks
            .iter()
            .find(|c| c.name == "escrita")
            .expect("escrita");
        assert_ne!(
            escrita.status,
            Status::Fail,
            "host de teste deve ser gravável"
        );
    }
}
