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
//! - **phase/aoi/grid/cell** — fluxo de monitoramento (Fase 1): fases, AOI,
//!   grade de células (motor puro `ta-engine`) e células bloqueadas.
//!
//! DSN: `--dsn` > env `TA_PG_DSN` > `postgres://ta:ta@localhost:5432/ta`.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use ta_db::pool::{self, DbConfig};

mod doctor;
mod dsn;
mod flow;
mod import;
mod migrate;
mod seed;

/// Resolve o DSN: flag → env → default do compose.
fn resolve_dsn(dsn: Option<&str>) -> String {
    dsn.map(str::to_owned)
        .or_else(|| std::env::var("TA_PG_DSN").ok())
        .unwrap_or_else(|| DbConfig::default().url)
}

/// Timeout de aquisição de conexão da CLI: curto de propósito. Com o banco
/// fora do ar, `migrate`/`seed-demo` (e o resto do CLI) devem falhar em ~5 s,
/// não nos 30 s do default do sqlx — mesmo racional do `doctor`.
pub(crate) const CLI_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// Abre o pool com o timeout curto da CLI, **sem** tocar no schema — para
/// chamadores que decidem o setup (ex.: `migrate` lê o estado das migrações
/// antes de aplicá-las).
pub(crate) async fn open_pool_raw(url: &str) -> sqlx::Result<sqlx::PgPool> {
    let cfg = DbConfig {
        url: url.to_owned(),
        max_connections: 4,
    };
    pool::connect_with_timeout(&cfg, CLI_ACQUIRE_TIMEOUT).await
}

async fn open_pool(dsn: Option<&str>) -> Result<sqlx::PgPool, String> {
    let url = resolve_dsn(dsn);
    let pool = open_pool_raw(&url)
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
    /// Cria o projeto `demo` com dados sintéticos (AOI + pontos + grade real).
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
    /// Fase do fluxo de monitoramento (Fase 1): criar/listar.
    Phase {
        #[command(subcommand)]
        cmd: PhaseCmd,
    },
    /// AOI — vincula uma camada existente como área de interesse.
    Aoi {
        #[command(subcommand)]
        cmd: AoiCmd,
    },
    /// Grade de células — gera a grade da AOI e consulta contagem/extensão.
    Grid {
        #[command(subcommand)]
        cmd: GridCmd,
    },
    /// Células — bloqueia e lista células bloqueadas.
    Cell {
        #[command(subcommand)]
        cmd: CellCmd,
    },
}

/// Subcomandos de `phase`.
#[derive(Debug, Subcommand)]
enum PhaseCmd {
    /// Cria uma fase no projeto.
    Create {
        /// Projeto alvo: id (UUID) ou nome — criado se o nome não existir.
        #[arg(long)]
        project: String,
        /// Nome da fase (único no projeto).
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// Ordem de exibição (padrão 0).
        #[arg(long, default_value_t = 0)]
        sort_order: i32,
        /// Marca a fase como bloqueadora de células.
        #[arg(long)]
        cell_blocker: bool,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista as fases do projeto.
    List {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `aoi`.
#[derive(Debug, Subcommand)]
enum AoiCmd {
    /// Vincula uma camada existente como AOI do projeto.
    Create {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Id (UUID) da camada que contém a geometria da AOI.
        #[arg(long)]
        layer: String,
        /// Coluna de atributo que identifica cada feição da AOI.
        #[arg(long)]
        id_column: Option<String>,
        /// Fase (id) dona da AOI; precisa ser do mesmo projeto.
        #[arg(long)]
        phase: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `grid`.
#[derive(Debug, Subcommand)]
enum GridCmd {
    /// Gera a grade da AOI (motor puro) e persiste camada + células.
    Create {
        /// Id (UUID) do vínculo de AOI (`aoi_layers.id`).
        #[arg(long)]
        aoi_layer: String,
        /// Resolução da grade NAS UNIDADES de --srid (graus se geográfico,
        /// metros se projetado).
        #[arg(long)]
        resolution: f64,
        /// Origem X do alinhamento (padrão: min_x da AOI), nas unidades de --srid.
        // `allow_hyphen_values`: longitude/latitude negativas são o caso comum
        // (ex.: `--origin-x -52.2`); sem isso só `--origin-x=-52.2` passaria.
        #[arg(long, allow_hyphen_values = true)]
        origin_x: Option<f64>,
        /// Origem Y do alinhamento (padrão: min_y da AOI), nas unidades de --srid.
        // Mesmo motivo do `origin_x` (ex.: `--origin-y -3.2`).
        #[arg(long, allow_hyphen_values = true)]
        origin_y: Option<f64>,
        /// EPSG do SRS em que --resolution/--origin-* são expressos: graus se
        /// geográfico (ex.: 4674 SIRGAS 2000), metros se projetado (ex.: 5880
        /// Policônica SIRGAS 2000). As células são sempre persistidas em
        /// EPSG:4674 (armazenamento canônico).
        // `allow_hyphen_values`: sem isso, `--srid -1` seria lido como flag
        // desconhecida por clap e só `--srid=-1` chegaria à validação, que é
        // quem sabe explicar o erro.
        #[arg(long, default_value_t = 4674, allow_hyphen_values = true)]
        srid: i32,
        /// Nome da camada de células (padrão `grade`).
        #[arg(long)]
        name: Option<String>,
        /// Fase (id) dona da grade; precisa ser do mesmo projeto.
        #[arg(long)]
        phase: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Mostra contagem, extensão e células bloqueadas da grade.
    Info {
        /// Id (UUID) da camada de células (`cell_layers.id`).
        #[arg(long)]
        cell_layer: String,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `cell`.
#[derive(Debug, Subcommand)]
enum CellCmd {
    /// Bloqueia uma célula da grade.
    Block {
        /// Id (UUID) da camada de células.
        #[arg(long)]
        cell_layer: String,
        /// Identificador da célula (`"<col>_<row>"`).
        #[arg(long)]
        cell_id: String,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista as células bloqueadas da grade.
    ListBlocked {
        /// Id (UUID) da camada de células.
        #[arg(long)]
        cell_layer: String,
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
        Cmd::Phase { cmd } => match cmd {
            PhaseCmd::Create {
                project,
                name,
                description,
                sort_order,
                cell_blocker,
                dsn,
            } => {
                flow::phase_create(
                    dsn.as_deref(),
                    &project,
                    &name,
                    description,
                    sort_order,
                    cell_blocker,
                )
                .await
            }
            PhaseCmd::List { project, dsn } => flow::phase_list(dsn.as_deref(), &project).await,
        },
        Cmd::Aoi { cmd } => match cmd {
            AoiCmd::Create {
                project,
                layer,
                id_column,
                phase,
                dsn,
            } => {
                flow::aoi_create(
                    dsn.as_deref(),
                    &project,
                    &layer,
                    id_column,
                    phase.as_deref(),
                )
                .await
            }
        },
        Cmd::Grid { cmd } => match cmd {
            GridCmd::Create {
                aoi_layer,
                resolution,
                origin_x,
                origin_y,
                srid,
                name,
                phase,
                dsn,
            } => {
                flow::grid_create(
                    dsn.as_deref(),
                    &aoi_layer,
                    resolution,
                    origin_x,
                    origin_y,
                    srid,
                    name,
                    phase.as_deref(),
                )
                .await
            }
            GridCmd::Info { cell_layer, dsn } => flow::grid_info(dsn.as_deref(), &cell_layer).await,
        },
        Cmd::Cell { cmd } => match cmd {
            CellCmd::Block {
                cell_layer,
                cell_id,
                reason,
                dsn,
            } => flow::cell_block(dsn.as_deref(), &cell_layer, &cell_id, reason).await,
            CellCmd::ListBlocked { cell_layer, dsn } => {
                flow::cell_list_blocked(dsn.as_deref(), &cell_layer).await
            }
        },
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

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;

    use super::*;

    /// `open_pool_raw` (caminho de `migrate`/`seed-demo`/demais) falha rápido
    /// quando o Postgres não responde: um listener que aceita a conexão TCP
    /// mas nunca fala o protocolo pendura o cliente no handshake. Sem o
    /// `CLI_ACQUIRE_TIMEOUT` de 5 s isso travaria os 30 s do default do sqlx.
    #[tokio::test]
    async fn pool_falha_rapido_com_host_que_nao_responde() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let port = listener.local_addr().expect("local_addr").port();
        // Aceita e segura a conexão sem responder nada (bloqueia no read).
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut buf = [0u8; 1024];
                    // Lê o que o cliente mandar mas **nunca responde**: o
                    // handshake fica pendurado até o timeout do cliente.
                    while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
                });
            }
        });

        let dsn = format!("postgres://ta:x@127.0.0.1:{port}/ta");
        let started = Instant::now();
        let err = open_pool_raw(&dsn)
            .await
            .expect_err("host que não responde deve falhar");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "deve falhar rápido (~5 s), levou {elapsed:?}: {err}"
        );
    }
}
