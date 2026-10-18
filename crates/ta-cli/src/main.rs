//! `ta-cli` — CLI do terraamazon-cleanroom-rust.
//!
//! Subcomandos (Fase 2):
//! - **import-vector** — detecta formato → lê → normaliza (4674) → cria
//!   camada → `bulk_insert` → `update_bbox`; reporta N features, bbox e
//!   tempo.
//! - **import-raster** — lê metadados (sem pixels) e registra a camada
//!   raster com `source_uri` + bbox do geotransform (opcional: derivar COG
//!   ao lado do arquivo).
//! - **export** — camada do banco → SHP/GeoJSON/GPKG/FGB/CSV.
//! - **list-layers** — camadas do projeto.
//! - **migrate** — aplica o schema (idempotente) e, com `--from-dir`, migra
//!   dados do legado (Fase 1; ver `migrate.rs`).
//! - **doctor** — diagnóstico do ambiente (banco/PostGIS/migrações/GDAL).
//! - **seed-demo** — projeto `demo` com dados sintéticos (ver `seed.rs`).
//!
//! DSN: `--dsn` > env `TA_PG_DSN` > `postgres://ta:ta@localhost:5432/ta`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use clap::{Parser, Subcommand};
use ta_db::pool::{self, DbConfig};

mod doctor;
mod dsn;
mod import;
mod migrate;
mod seed;

/// Resolve o DSN: flag → env → default do compose.
fn resolve_dsn(dsn: Option<&str>) -> String {
    dsn.map(str::to_owned)
        .or_else(|| std::env::var("TA_PG_DSN").ok())
        .unwrap_or_else(|| DbConfig::default().url)
}

async fn open_pool(dsn: Option<&str>) -> Result<sqlx::PgPool, String> {
    let url = resolve_dsn(dsn);
    let cfg = DbConfig {
        url: url.clone(),
        max_connections: 4,
    };
    let pool = pool::connect(&cfg)
        .await
        .map_err(|e| format!("conexão falhou ({}): {e}", dsn::mask_password(&url)))?;
    // Setup idempotente: extensão postgis + migrações (primeiro uso num
    // banco novo não deve exigir passo manual).
    ta_db::spatial::ensure_postgis(&pool)
        .await
        .map_err(|e| e.to_string())?;
    ta_db::spatial::run_migrations(&pool)
        .await
        .map_err(|e| format!("migrações: {e}"))?;
    Ok(pool)
}

#[derive(Debug, Parser)]
#[command(
    name = "ta-cli",
    version,
    about = "CLI do terraamazon-cleanroom-rust (GIS clean-room tipo TerraAmazon)",
    after_help = "DSN do banco: --dsn > env TA_PG_DSN > postgres://ta:ta@localhost:5432/ta"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Importa uma fonte vetorial (SHP/GeoJSON/GPKG/FGB/CSV-XY) para o banco.
    ImportVector {
        /// Arquivo de entrada (formato detectado por extensão/magic).
        #[arg(long)]
        input: PathBuf,
        /// Projeto alvo: id (UUID) ou nome — criado se o nome não existir.
        /// Omitido → projeto "default".
        #[arg(long)]
        project: Option<String>,
        /// Nome da camada (padrão: stem do arquivo; sufixo _N se existir).
        #[arg(long)]
        layer_name: Option<String>,
        /// SRID declarado da origem quando o arquivo não carrega CRS.
        #[arg(long)]
        srid: Option<u32>,
        /// CSV-XY: coluna de longitude (padrão `lon`).
        #[arg(long)]
        lon_col: Option<String>,
        /// CSV-XY: coluna de latitude (padrão `lat`).
        #[arg(long)]
        lat_col: Option<String>,
        /// CSV-XY: separador explícito (padrão: sniff , ; \t).
        #[arg(long)]
        sep: Option<char>,
        /// CSV-XY: encoding do arquivo (utf8|latin1; padrão utf8).
        #[arg(long, default_value = "utf8")]
        encoding: String,
        /// DSN do banco (senão env TA_PG_DSN / default compose).
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Registra um raster (GeoTIFF/COG) — metadados apenas, sem pixels no PG.
    ImportRaster {
        /// Arquivo de entrada.
        #[arg(long)]
        input: PathBuf,
        /// Projeto alvo: id (UUID) ou nome — criado se o nome não existir.
        #[arg(long)]
        project: Option<String>,
        /// Nome da camada (padrão: stem do arquivo).
        #[arg(long)]
        layer_name: Option<String>,
        /// Opcional: também produz COG ao lado (gdal_translate -of COG).
        #[arg(long)]
        cog: Option<PathBuf>,
        /// DSN do banco.
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Exporta uma camada do banco para arquivo (SHP/GeoJSON/GPKG/FGB/CSV).
    Export {
        /// Id (UUID) da camada.
        #[arg(long)]
        layer: String,
        /// Formato de saída.
        #[arg(long, value_parser = ["shp", "geojson", "gpkg", "fgb", "csv"])]
        format: String,
        /// Arquivo de saída (sobrescreve).
        #[arg(long)]
        output: PathBuf,
        /// SRID de saída (padrão 4674; ≠4674 exige build com gdal).
        #[arg(long, default_value = "4674")]
        srid_out: u32,
        /// DSN do banco.
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista as camadas do projeto (id ou nome; padrão: todos os projetos).
    ListLayers {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: Option<String>,
        /// DSN do banco.
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Migra dados do banco legado/diretório de fontes para PG16/PostGIS (SRID 4674).
    ///
    /// Sem `--from-dir`, apenas aplica extensão + migrações do schema
    /// (idempotente) no destino.
    Migrate {
        /// Diretório com os arquivos de origem (.shp/.tif/.sql). Omitido →
        /// modo schema-only.
        #[arg(long)]
        from_dir: Option<PathBuf>,
        /// DSN do destino, ex.: postgres://ta:ta@host:5432/ta (senão env
        /// TA_PG_DSN / default do compose).
        #[arg(long)]
        to: Option<String>,
        /// Só lista o que seria feito; não executa nada.
        #[arg(long)]
        dry_run: bool,
    },
    /// Diagnostica o ambiente (PostgreSQL, PostGIS, migrações, GDAL, escrita).
    ///
    /// Exit 0 = tudo OK; exit 1 = alguma checagem falhou, com o que fazer.
    Doctor {
        /// DSN do banco (senão env TA_PG_DSN / default compose).
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Cria o projeto `demo` com dados sintéticos (AOI + pontos + grade provisória).
    SeedDemo {
        /// Projeto alvo (criado se não existir).
        #[arg(long, default_value = seed::DEMO_PROJECT)]
        project: String,
        /// Polígono da AOI (GeoJSON EPSG:4674).
        #[arg(long, default_value = "testdata/monitoring/aoi.geojson")]
        aoi: PathBuf,
        /// Pontos com atributo de classe (CSV-XY).
        #[arg(long, default_value = "testdata/monitoring/points_classes.csv")]
        points: PathBuf,
        /// DSN do banco (senão env TA_PG_DSN / default compose).
        #[arg(long)]
        dsn: Option<String>,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let started = Instant::now();
    let result = match cli.cmd {
        Cmd::ImportVector {
            input,
            project,
            layer_name,
            srid,
            lon_col,
            lat_col,
            sep,
            encoding,
            dsn,
        } => {
            let enc = match encoding.to_ascii_lowercase().as_str() {
                "utf8" | "utf-8" => ta_gis::reader::CsvEncoding::Utf8,
                "latin1" | "iso-8859-1" => ta_gis::reader::CsvEncoding::Latin1,
                other => {
                    eprintln!("import-vector: encoding inválido `{other}` (utf8|latin1)");
                    return ExitCode::from(2);
                }
            };
            import::import_vector(import::ImportVectorArgs {
                input,
                project,
                layer_name,
                srid,
                lon_col,
                lat_col,
                sep,
                encoding: enc,
                dsn,
            })
            .await
        }
        Cmd::ImportRaster {
            input,
            project,
            layer_name,
            cog,
            dsn,
        } => {
            import::import_raster(import::ImportRasterArgs {
                input,
                project,
                layer_name,
                cog,
                dsn,
            })
            .await
        }
        Cmd::Export {
            layer,
            format,
            output,
            srid_out,
            dsn,
        } => {
            import::export_layer(import::ExportArgs {
                layer,
                format,
                output,
                srid_out,
                dsn,
            })
            .await
        }
        Cmd::ListLayers { project, dsn } => list_layers(project, dsn).await,
        Cmd::Migrate {
            from_dir,
            to,
            dry_run,
        } => {
            migrate::run(migrate::MigrateArgs {
                from_dir,
                to: resolve_dsn(to.as_deref()),
                dry_run,
            })
            .await
        }
        // `doctor` imprime o próprio relatório e devolve o exit code — não
        // passa pelo formatador de erro ("erro (... ms): ...") abaixo.
        Cmd::Doctor { dsn } => {
            let report = doctor::diagnose(dsn.as_deref()).await;
            print!("{}", report.render());
            return ExitCode::from(report.exit_code());
        }
        Cmd::SeedDemo {
            project,
            aoi,
            points,
            dsn,
        } => {
            seed::run(seed::SeedArgs {
                project,
                aoi,
                points,
                dsn,
            })
            .await
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erro ({} ms): {e}", started.elapsed().as_millis());
            ExitCode::FAILURE
        }
    }
}

/// Lista camadas (por projeto ou todas), com origem.
async fn list_layers(project: Option<String>, dsn: Option<String>) -> Result<(), String> {
    use ta_db::repo::{LayerRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
    let pool = open_pool(dsn.as_deref()).await?;
    let layers = PgLayerRepo::new(pool.clone());
    let projects = PgProjectRepo::new(pool.clone());

    match project {
        Some(p) => {
            let proj = resolve_project(&projects, &p).await?;
            let list = layers
                .list_for_project(&proj.id)
                .await
                .map_err(|e| e.to_string())?;
            if list.is_empty() {
                println!("(nenhuma camada no projeto `{}`)", proj.name);
            }
            for l in &list {
                print_layer(&proj.name, l);
            }
        }
        None => {
            for proj in projects.list().await.map_err(|e| e.to_string())? {
                for l in layers
                    .list_for_project(&proj.id)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    print_layer(&proj.name, &l);
                }
            }
        }
    }
    pool.close().await;
    Ok(())
}

fn print_layer(proj: &str, l: &ta_core::layer::Layer) {
    println!(
        "projeto={proj}\t{}\t{:?}\tsrid={}\tsource={}",
        l.id, l.geom_type, l.srid, l.source_uri
    );
}

use ta_db::repo::ProjectRepo;

/// Resolve `id (UUID)` ou `nome` para um projeto; cria se for nome novo.
pub(crate) async fn resolve_project<R>(
    repo: &R,
    id_or_name: &str,
) -> Result<ta_core::project::Project, String>
where
    R: ProjectRepo,
{
    let looks_uuid = id_or_name.len() == 36 && id_or_name.as_bytes()[8] == b'-';
    if looks_uuid {
        if let Some(p) = repo.get(id_or_name).await.map_err(|e| e.to_string())? {
            return Ok(p);
        }
    }
    for p in repo.list().await.map_err(|e| e.to_string())? {
        if p.name == id_or_name {
            return Ok(p);
        }
    }
    repo.create(&ta_core::project::Project {
        id: String::new(),
        name: id_or_name.to_string(),
        srid: 4674,
        created_at: time::OffsetDateTime::now_utc(),
    })
    .await
    .map_err(|e| format!("criar projeto `{id_or_name}`: {e}"))
}
