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
mod flow_area;
mod flow_exchange;
mod flow_scene;
mod flow_task;
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
    /// Classes — CRUD de classes e dos grupos de classes (Fase 2).
    Class {
        #[command(subcommand)]
        cmd: ClassCmd,
    },
    /// Camadas de rascunho de classificação (Fase 2).
    Draft {
        #[command(subcommand)]
        cmd: DraftCmd,
    },
    /// Rulesets versionados — criar/listar/mostrar (Fase 2).
    Ruleset {
        #[command(subcommand)]
        cmd: RulesetCmd,
    },
    /// Regras de classificação — adicionar/listar/remover/avaliar (Fase 2).
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
    /// Medidas observadas por célula — set/list/clear (Fase 3).
    Measure {
        #[command(subcommand)]
        cmd: MeasureCmd,
    },
    /// Fila de processos — create/run/list/show/cancel/retry (Fase 3).
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Processos de uma task — lista por estado (Fase 3).
    Process {
        #[command(subcommand)]
        cmd: ProcessCmd,
    },
    /// Cenas de sensoriamento — add/list/link/unlink/remove (Fase 4).
    Scene {
        #[command(subcommand)]
        cmd: SceneCmd,
    },
    /// Exchange do fluxo de monitoramento — export/import (Fase 4).
    Exchange {
        #[command(subcommand)]
        cmd: ExchangeCmd,
    },
    /// Cálculo de área consolidado — relatório por grade/rascunho (Fase 5).
    ///
    /// A área é calculada no SRS MÉTRICO de `area_settings.srs_id` do projeto
    /// (default EPSG:5880 — Decisão 4); nunca em graus (4674 é só
    /// armazenamento).
    Area {
        #[command(subcommand)]
        cmd: AreaCmd,
    },
    /// Projetos — deleção completa com relatório (Fase 4).
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
}

/// Subcomandos de `area` (Fase 5).
#[derive(Debug, Subcommand)]
enum AreaCmd {
    /// Relatório de área da grade: total, bloqueada, disponível (em ha) —
    /// e por classe do rascunho, com `--draft`. O SRS usado é impresso
    /// sempre; `--srs` sobrepõe `area_settings.srs_id` (default 5880).
    /// SRS geográfico (graus) é recusado: área nunca é calculada em graus.
    Report {
        /// Id (UUID) da camada de células.
        #[arg(long)]
        cell_layer: String,
        /// Nome do rascunho classificado — acrescenta a área por classe.
        #[arg(long)]
        draft: Option<String>,
        /// EPSG métrico do cálculo (padrão: `area_settings.srs_id` do
        /// projeto; sem linha, 5880 Policônica SIRGAS 2000).
        #[arg(long)]
        srs: Option<i32>,
        /// Saída em JSON (mesmo contrato para CLI e GUI).
        #[arg(long)]
        json: bool,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `project`.
#[derive(Debug, Subcommand)]
enum ProjectCmd {
    /// Apaga o projeto INTEIRO (DESTRUTIVO): deleção ordenada em transação
    /// única com relatório de contagens por tabela; sem --yes mostra o
    /// resumo do que seria apagado e recusa. Cenas globais permanecem.
    Delete {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Confirma a remoção.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `scene`.
#[derive(Debug, Subcommand)]
enum SceneCmd {
    /// Registra uma cena (global; nome é único no banco).
    Add {
        /// Nome da cena (ex.: `S2A_2026-01-31`).
        #[arg(long)]
        name: String,
        /// Aquisição em ISO-8601 (`2026-01-31` ou `2026-01-31T12:00:00Z`).
        #[arg(long)]
        acquisition: Option<String>,
        /// Footprint: WKT (POLYGON/MULTIPOLYGON em EPSG:4674) ou
        /// arquivo .wkt/.geojson.
        #[arg(long)]
        footprint: Option<String>,
        /// Metadados livres como objeto JSON (`'{"satellite":"S2"}'`).
        #[arg(long)]
        metadata: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista cenas — todas, ou só as de um projeto (--project).
    List {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Vincula a cena ao projeto (idempotente).
    Link {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome da cena.
        #[arg(long)]
        scene: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Desvincula a cena do projeto (a cena continua existindo).
    Unlink {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome da cena.
        #[arg(long)]
        scene: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Apaga a cena (DESTRUTIVO; vínculos caem, medidas perdem a proveniência
    /// mas permanecem) — sem --yes mostra as contagens e recusa.
    Remove {
        /// Nome da cena.
        #[arg(long)]
        name: String,
        /// Confirma a remoção.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `exchange`.
#[derive(Debug, Subcommand)]
enum ExchangeCmd {
    /// Exporta TODAS as seções do projeto para um exchange
    /// `curupira-monitoring-exchange v1` (JSON pretty).
    Export {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Arquivo de saída (JSON; sobrescreve).
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Importa um exchange criando um projeto NOVO (recusa nome existente —
    /// sem merge nesta fase).
    Import {
        /// Arquivo do exchange (JSON).
        #[arg(long = "in", visible_alias = "input")]
        input: PathBuf,
        /// Nome do projeto NOVO a criar.
        #[arg(long)]
        into: String,
        #[arg(long)]
        dsn: Option<String>,
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
    /// Mostra o log de execução de uma célula (fila de processos, Fase 3).
    Log {
        /// Id (UUID) da camada de células.
        #[arg(long)]
        cell_layer: String,
        /// Identificador da célula (`"<col>_<row>"`).
        #[arg(long)]
        cell_id: String,
        /// Filtra por task (id UUID).
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `class`.
#[derive(Debug, Subcommand)]
enum ClassCmd {
    /// Cria uma classe no projeto.
    Create {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome da classe (único no projeto).
        #[arg(long)]
        name: String,
        /// Grupo (id ou nome) do mesmo projeto.
        #[arg(long)]
        group: Option<String>,
        /// Cor de exibição no formato #RRGGBB.
        #[arg(long)]
        color: Option<String>,
        /// Classe-mãe (id ou nome) do mesmo projeto.
        #[arg(long)]
        parent: Option<String>,
        /// Ordem de exibição (padrão 0).
        #[arg(long, default_value_t = 0)]
        sort_order: i32,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista as classes do projeto.
    List {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Atualiza nome/grupo/cor/classe-mãe/atividade de uma classe.
    Update {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Classe alvo: id (UUID) ou nome.
        #[arg(long)]
        class: String,
        /// Novo nome.
        #[arg(long)]
        name: Option<String>,
        /// Novo grupo (id ou nome).
        #[arg(long)]
        group: Option<String>,
        /// Nova cor #RRGGBB.
        #[arg(long)]
        color: Option<String>,
        /// Nova classe-mãe (id ou nome).
        #[arg(long)]
        parent: Option<String>,
        /// Ativa/desativa a classe (hoje atributo de EXIBIÇÃO: o motor de
        /// regras não consulta classes).
        #[arg(long)]
        active: Option<bool>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Remove uma classe; bloqueia se houver regras apontando para ela
    /// (mostra a contagem), salvo --force (regras + classe numa transação).
    Delete {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Classe alvo: id (UUID) ou nome.
        #[arg(long)]
        class: String,
        /// Apaga TAMBÉM as regras que decidem a classe, numa única transação.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Grupos de classes.
    Group {
        #[command(subcommand)]
        cmd: ClassGroupCmd,
    },
}

/// Subcomandos de `class group`.
#[derive(Debug, Subcommand)]
enum ClassGroupCmd {
    /// Cria um grupo de classes.
    Create {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome do grupo (único no projeto).
        #[arg(long)]
        name: String,
        /// Ordem de exibição (padrão 0).
        #[arg(long, default_value_t = 0)]
        sort_order: i32,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista os grupos do projeto.
    List {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Renomeia um grupo.
    Rename {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Grupo alvo: id (UUID) ou nome.
        #[arg(long)]
        group: String,
        /// Novo nome.
        #[arg(long)]
        name: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Remove um grupo (classes ficam sem grupo).
    Delete {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Grupo alvo: id (UUID) ou nome.
        #[arg(long)]
        group: String,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `draft`.
#[derive(Debug, Subcommand)]
enum DraftCmd {
    /// Cria uma camada de rascunho de classificação.
    Create {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome do rascunho (único no projeto).
        #[arg(long)]
        name: String,
        /// Fase (id) do mesmo projeto.
        #[arg(long)]
        phase: Option<String>,
        /// Camada de células (id) do mesmo projeto.
        #[arg(long)]
        cell_layer: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista os rascunhos do projeto.
    List {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Apaga o rascunho e as células classificadas nele (CASCADE) — sem
    /// --yes bloqueia mostrando a contagem (destrutivo; intenção explícita).
    Delete {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome do rascunho.
        #[arg(long)]
        name: String,
        /// Confirma a remoção (o rascunho é derivado, mas apagar decisões
        /// classificadas é destrutivo).
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `ruleset`.
#[derive(Debug, Subcommand)]
enum RulesetCmd {
    /// Cria a próxima versão de um ruleset.
    Create {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome do ruleset (a linhagem é versionada por nome).
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista os rulesets do projeto (todas as versões).
    List {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Mostra um ruleset (versão mais recente por padrão) e suas regras.
    Show {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome do ruleset.
        #[arg(long)]
        name: String,
        /// Versão específica (padrão: a mais recente).
        #[arg(long)]
        version: Option<i32>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Torna este ruleset o ATIVO do projeto — o default do `task create`
    /// (desativa os demais; no máximo um ativo por projeto).
    Activate {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome do ruleset.
        #[arg(long)]
        name: String,
        /// Versão específica (padrão: a mais recente da linhagem).
        #[arg(long)]
        version: Option<i32>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Remove um ruleset (regras caem em cascata). Bloqueia se houver células
    /// classificadas decididas por ele (mostra contagem/nomes), salvo
    /// --force (decisões + regras + ruleset numa única transação).
    Delete {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Nome do ruleset.
        #[arg(long)]
        name: String,
        /// Versão específica (padrão: a mais recente da linhagem).
        #[arg(long)]
        version: Option<i32>,
        /// Apaga TAMBÉM as decisões de rascunho que o ruleset decidiu, numa
        /// única transação.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `rule`.
#[derive(Debug, Subcommand)]
enum RuleCmd {
    /// Adiciona uma regra a um ruleset.
    Add {
        /// Id (UUID) do ruleset.
        #[arg(long)]
        ruleset: String,
        /// Classe decidida: id (UUID) ou nome do mesmo projeto do ruleset.
        #[arg(long)]
        class: String,
        /// Nome da regra (único no ruleset).
        #[arg(long)]
        name: String,
        /// Prioridade (maior vence; empate desempata pelo menor nome).
        #[arg(long, default_value_t = 0)]
        priority: i32,
        /// Predicado, ex.: `ndvi >= 0.6 and nbr < 0.1`.
        #[arg(long)]
        when: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista as regras de um ruleset (maior prioridade primeiro).
    List {
        /// Id (UUID) do ruleset.
        #[arg(long)]
        ruleset: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Remove uma regra pelo nome.
    Remove {
        /// Id (UUID) do ruleset.
        #[arg(long)]
        ruleset: String,
        /// Nome da regra.
        #[arg(long)]
        name: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Atualiza parcialmente uma regra — apenas as flags presentes mudam.
    Update {
        /// Id (UUID) do ruleset.
        #[arg(long)]
        ruleset: String,
        /// Nome da regra (identifica a regra dentro do ruleset).
        #[arg(long)]
        name: String,
        /// Nova classe decidida: id (UUID) ou nome do mesmo projeto.
        #[arg(long)]
        class: Option<String>,
        /// Nova prioridade (maior vence).
        #[arg(long)]
        priority: Option<i32>,
        /// Novo predicado, ex.: `ndvi >= 0.6 and nbr < 0.1`.
        #[arg(long)]
        when: Option<String>,
        /// Ativa/desativa a regra (inativa é ignorada na avaliação).
        #[arg(long)]
        active: Option<bool>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Avalia o ruleset contra medidas observadas e imprime a decisão.
    Eval {
        /// Id (UUID) do ruleset.
        #[arg(long)]
        ruleset: String,
        /// Medidas no formato `nome=valor,nome=valor` (ex.: `ndvi=0.7,nbr=0.05`).
        #[arg(long, allow_hyphen_values = true)]
        obs: String,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `measure`.
#[derive(Debug, Subcommand)]
enum MeasureCmd {
    /// Grava (upsert) as medidas de UMA célula.
    Set {
        /// Id (UUID) da camada de células.
        #[arg(long)]
        cell_layer: String,
        /// Identificador da célula (`"<col>_<row>"`).
        #[arg(long)]
        cell_id: String,
        /// Medidas no formato `nome=valor,nome=valor` (ex.: `ndvi=0.7,nbr=0.05`).
        #[arg(long, allow_hyphen_values = true)]
        values: String,
        /// Proveniência livre (ex.: cena/índice de origem).
        #[arg(long)]
        source: Option<String>,
        /// Cena de origem (por NOME; gravada como proveniência na medida).
        #[arg(long)]
        scene: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista as medidas da camada (opcionalmente de uma célula).
    List {
        /// Id (UUID) da camada de células.
        #[arg(long)]
        cell_layer: String,
        /// Filtra por célula.
        #[arg(long)]
        cell_id: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Remove as medidas da camada (opcionalmente de uma célula).
    Clear {
        /// Id (UUID) da camada de células.
        #[arg(long)]
        cell_layer: String,
        /// Filtra por célula.
        #[arg(long)]
        cell_id: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Importa medidas de um CSV LONG (`cell_id,measure,value[,scene]`).
    ///
    /// Mesmo caminho da GUI (`ta_flow::measures::import_csv`): valida número
    /// finito, célula da grade e cena existente; linhas inválidas são
    /// IGNORADAS com motivo (o relatório mostra lidas/gravadas/ignoradas).
    ImportCsv {
        /// Id (UUID) da camada de células.
        #[arg(long)]
        cell_layer: String,
        /// Caminho do CSV (cabeçalho opcional; vírgula, sem aspas).
        #[arg(long)]
        input: String,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `task`.
#[derive(Debug, Subcommand)]
enum TaskCmd {
    /// Enfileira uma task de classificação por célula (Fase 3).
    Create {
        /// Tipo de trabalho (só `classify` nesta fase).
        #[arg(value_parser = ["classify"])]
        kind: String,
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: String,
        /// Id (UUID) da camada de células alvo.
        #[arg(long)]
        cell_layer: String,
        /// Id (UUID) do ruleset (padrão: o ATIVO do projeto).
        #[arg(long)]
        ruleset: Option<String>,
        /// Nome do rascunho (padrão `rascunho <ruleset>`; existente é
        /// reaproveitado — reclassificação é upsert).
        #[arg(long)]
        draft_layer: Option<String>,
        /// Nome de exibição da task.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Processa a task em lotes transacionais (retomável; `--limit` para
    /// processar no máximo N células nesta invocação).
    Run {
        /// Id (UUID) da task.
        task: String,
        /// Máximo de células a processar nesta invocação.
        #[arg(long)]
        limit: Option<u64>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Lista as tasks do projeto (ou todas, sem --project).
    List {
        /// Projeto alvo: id (UUID) ou nome.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Mostra a task e as contagens de processos por estado.
    Show {
        /// Id (UUID) da task.
        task: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Cancela cooperativamente (o lote em voo termina; pendentes nunca mais
    /// são reclamados).
    Cancel {
        /// Id (UUID) da task.
        task: String,
        #[arg(long)]
        dsn: Option<String>,
    },
    /// Repõe os processos com erro na fila (error → pending; `done` nunca é
    /// reprocessado).
    Retry {
        /// Id (UUID) da task.
        task: String,
        #[arg(long)]
        dsn: Option<String>,
    },
}

/// Subcomandos de `process`.
#[derive(Debug, Subcommand)]
enum ProcessCmd {
    /// Lista os processos da task (opcionalmente por estado).
    List {
        /// Id (UUID) da task.
        #[arg(long)]
        task: String,
        /// Filtra por estado (pending|running|done|error).
        #[arg(long)]
        status: Option<String>,
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
            CellCmd::Log {
                cell_layer,
                cell_id,
                task,
                dsn,
            } => flow_task::cell_log(dsn.as_deref(), &cell_layer, &cell_id, task.as_deref()).await,
        },
        Cmd::Class { cmd } => match cmd {
            ClassCmd::Create {
                project,
                name,
                group,
                color,
                parent,
                sort_order,
                dsn,
            } => {
                flow::class_create(
                    dsn.as_deref(),
                    &project,
                    &name,
                    group.as_deref(),
                    color.as_deref(),
                    parent.as_deref(),
                    sort_order,
                )
                .await
            }
            ClassCmd::List { project, dsn } => flow::class_list(dsn.as_deref(), &project).await,
            ClassCmd::Update {
                project,
                class,
                name,
                group,
                color,
                parent,
                active,
                dsn,
            } => {
                flow::class_update(
                    dsn.as_deref(),
                    &project,
                    &class,
                    name,
                    group,
                    color,
                    parent,
                    active,
                )
                .await
            }
            ClassCmd::Delete {
                project,
                class,
                force,
                dsn,
            } => flow::class_delete(dsn.as_deref(), &project, &class, force).await,
            ClassCmd::Group { cmd } => match cmd {
                ClassGroupCmd::Create {
                    project,
                    name,
                    sort_order,
                    dsn,
                } => flow::class_group_create(dsn.as_deref(), &project, &name, sort_order).await,
                ClassGroupCmd::List { project, dsn } => {
                    flow::class_group_list(dsn.as_deref(), &project).await
                }
                ClassGroupCmd::Rename {
                    project,
                    group,
                    name,
                    dsn,
                } => flow::class_group_rename(dsn.as_deref(), &project, &group, &name).await,
                ClassGroupCmd::Delete {
                    project,
                    group,
                    dsn,
                } => flow::class_group_delete(dsn.as_deref(), &project, &group).await,
            },
        },
        Cmd::Draft { cmd } => match cmd {
            DraftCmd::Create {
                project,
                name,
                phase,
                cell_layer,
                dsn,
            } => {
                flow::draft_create(
                    dsn.as_deref(),
                    &project,
                    &name,
                    phase.as_deref(),
                    cell_layer.as_deref(),
                )
                .await
            }
            DraftCmd::List { project, dsn } => flow::draft_list(dsn.as_deref(), &project).await,
            DraftCmd::Delete {
                project,
                name,
                yes,
                dsn,
            } => flow::draft_delete(dsn.as_deref(), &project, &name, yes).await,
        },
        Cmd::Ruleset { cmd } => match cmd {
            RulesetCmd::Create {
                project,
                name,
                description,
                dsn,
            } => flow::ruleset_create(dsn.as_deref(), &project, &name, description).await,
            RulesetCmd::List { project, dsn } => flow::ruleset_list(dsn.as_deref(), &project).await,
            RulesetCmd::Show {
                project,
                name,
                version,
                dsn,
            } => flow::ruleset_show(dsn.as_deref(), &project, &name, version).await,
            RulesetCmd::Activate {
                project,
                name,
                version,
                dsn,
            } => flow_task::ruleset_activate(dsn.as_deref(), &project, &name, version).await,
            RulesetCmd::Delete {
                project,
                name,
                version,
                force,
                dsn,
            } => flow::ruleset_delete(dsn.as_deref(), &project, &name, version, force).await,
        },
        Cmd::Rule { cmd } => match cmd {
            RuleCmd::Add {
                ruleset,
                class,
                name,
                priority,
                when,
                dsn,
            } => flow::rule_add(dsn.as_deref(), &ruleset, &class, &name, priority, &when).await,
            RuleCmd::List { ruleset, dsn } => flow::rule_list(dsn.as_deref(), &ruleset).await,
            RuleCmd::Remove { ruleset, name, dsn } => {
                flow::rule_remove(dsn.as_deref(), &ruleset, &name).await
            }
            RuleCmd::Update {
                ruleset,
                name,
                class,
                priority,
                when,
                active,
                dsn,
            } => {
                flow::rule_update(
                    dsn.as_deref(),
                    &ruleset,
                    &name,
                    class,
                    priority,
                    when,
                    active,
                )
                .await
            }
            RuleCmd::Eval { ruleset, obs, dsn } => {
                flow::rule_eval(dsn.as_deref(), &ruleset, &obs).await
            }
        },
        Cmd::Measure { cmd } => match cmd {
            MeasureCmd::Set {
                cell_layer,
                cell_id,
                values,
                source,
                scene,
                dsn,
            } => {
                flow_task::measure_set(
                    dsn.as_deref(),
                    &cell_layer,
                    &cell_id,
                    &values,
                    source,
                    scene.as_deref(),
                )
                .await
            }
            MeasureCmd::List {
                cell_layer,
                cell_id,
                dsn,
            } => flow_task::measure_list(dsn.as_deref(), &cell_layer, cell_id.as_deref()).await,
            MeasureCmd::Clear {
                cell_layer,
                cell_id,
                dsn,
            } => flow_task::measure_clear(dsn.as_deref(), &cell_layer, cell_id.as_deref()).await,
            MeasureCmd::ImportCsv {
                cell_layer,
                input,
                dsn,
            } => flow_task::measure_import_csv(dsn.as_deref(), &cell_layer, &input).await,
        },
        Cmd::Task { cmd } => match cmd {
            TaskCmd::Create {
                kind,
                project,
                cell_layer,
                ruleset,
                draft_layer,
                name,
                dsn,
            } => {
                if kind != "classify" {
                    eprintln!(
                        "kind inválido `{kind}`: nesta fase só existe `task create classify`"
                    );
                    return ExitCode::from(2);
                }
                flow_task::task_create(
                    dsn.as_deref(),
                    &project,
                    &cell_layer,
                    ruleset.as_deref(),
                    draft_layer.as_deref(),
                    name.as_deref(),
                )
                .await
            }
            TaskCmd::Run { task, limit, dsn } => {
                flow_task::task_run(dsn.as_deref(), &task, limit).await
            }
            TaskCmd::List { project, dsn } => {
                flow_task::task_list(dsn.as_deref(), project.as_deref()).await
            }
            TaskCmd::Show { task, dsn } => flow_task::task_show(dsn.as_deref(), &task).await,
            TaskCmd::Cancel { task, dsn } => flow_task::task_cancel(dsn.as_deref(), &task).await,
            TaskCmd::Retry { task, dsn } => flow_task::task_retry(dsn.as_deref(), &task).await,
        },
        Cmd::Process { cmd } => match cmd {
            ProcessCmd::List { task, status, dsn } => {
                flow_task::process_list(dsn.as_deref(), &task, status.as_deref()).await
            }
        },
        Cmd::Scene { cmd } => match cmd {
            SceneCmd::Add {
                name,
                acquisition,
                footprint,
                metadata,
                dsn,
            } => {
                flow_scene::scene_add(dsn.as_deref(), &name, acquisition, footprint, metadata).await
            }
            SceneCmd::List { project, dsn } => {
                flow_scene::scene_list(dsn.as_deref(), project).await
            }
            SceneCmd::Link {
                project,
                scene,
                dsn,
            } => flow_scene::scene_link(dsn.as_deref(), &project, &scene).await,
            SceneCmd::Unlink {
                project,
                scene,
                dsn,
            } => flow_scene::scene_unlink(dsn.as_deref(), &project, &scene).await,
            SceneCmd::Remove { name, yes, dsn } => {
                flow_scene::scene_remove(dsn.as_deref(), &name, yes).await
            }
        },
        Cmd::Exchange { cmd } => match cmd {
            ExchangeCmd::Export { project, out, dsn } => {
                flow_exchange::exchange_export(dsn.as_deref(), &project, &out).await
            }
            ExchangeCmd::Import { input, into, dsn } => {
                flow_exchange::exchange_import(dsn.as_deref(), &input, &into).await
            }
        },
        Cmd::Area { cmd } => match cmd {
            AreaCmd::Report {
                cell_layer,
                draft,
                srs,
                json,
                dsn,
            } => {
                flow_area::area_report(dsn.as_deref(), &cell_layer, draft.as_deref(), srs, json)
                    .await
            }
        },
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::Delete { project, yes, dsn } => {
                flow::project_delete(dsn.as_deref(), &project, yes).await
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
    // F12: nome vazio/só-espaços auto-criaria um projeto sem nome — rejeita
    // antes de tocar no banco, com erro acionável.
    if id_or_name.trim().is_empty() {
        return Err(
            "nome de projeto vazio: informe --project com um nome (ex.: --project demo) \
             ou um id (UUID) de projeto existente"
                .to_owned(),
        );
    }
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

    use async_trait::async_trait;
    use ta_core::project::Project;
    use ta_db::repo::ProjectRepo;

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

    /// Mock mínimo de `ProjectRepo` (sem banco): se `resolve_project`
    /// tentasse CRIAR, `create` entra em pânico — o teste prova que o nome
    /// vazio é rejeitado antes de qualquer escrita (F12).
    struct RepoSemEscrita;
    #[async_trait]
    impl ProjectRepo for RepoSemEscrita {
        async fn create(&self, _project: &Project) -> ta_db::Result<Project> {
            panic!("resolve_project não pode criar projeto com nome vazio");
        }
        async fn get(&self, _id: &str) -> ta_db::Result<Option<Project>> {
            Ok(None)
        }
        async fn list(&self) -> ta_db::Result<Vec<Project>> {
            Ok(Vec::new())
        }
        async fn delete(&self, _id: &str) -> ta_db::Result<bool> {
            Ok(false)
        }
        async fn rename(&self, _id: &str, _new_name: &str) -> ta_db::Result<bool> {
            Ok(false)
        }
    }

    /// F12: `resolve_project("")` (ou só espaços) rejeita com erro acionável
    /// em vez de auto-criar um projeto sem nome.
    #[tokio::test]
    async fn resolve_project_rejeita_nome_vazio() {
        for vazio in ["", "   "] {
            let err = resolve_project(&RepoSemEscrita, vazio)
                .await
                .expect_err("nome vazio deve ser rejeitado");
            assert!(
                err.contains("nome de projeto vazio") && err.contains("--project"),
                "mensagem acionável ausente para {vazio:?}: {err}"
            );
        }
    }
}
