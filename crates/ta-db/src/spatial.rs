//! Utilitários espaciais do banco (PostGIS).
//!
//! O DDL real vive em `crates/ta-db/migrations/` (aplicado por [`run_migrations`]):
//! `projects` / `layers` / `features` com `geometry(Geometry, 4674)`,
//! índice GIST e CHECK de SRID. Aqui ficam só os helpers de runtime:
//! disponibilidade do PostGIS, aplicação das migrações e **tiles MVT**
//! (`ST_AsMVT` — consumido pela GUI/`ta-gis::derived`).
//!
//! Todas as queries são **bind-only** (parâmetros `$n`, zero interpolação
//! de dado no SQL) — gate themis.

use sqlx::PgPool;

use crate::{Error, Result};
use ta_core::project::DEFAULT_SRID;

/// SRID-alvo gravado no CHECK do schema.
pub const TARGET_SRID: u32 = DEFAULT_SRID;

/// Extensão geográfica do mundo XYZ (graus, EPSG:4674 é geográfico e o
/// schema enforce 4674 — o tile scheme usa lon/lat nesse envelope).
const WORLD_BOUNDS: [f64; 4] = [-180.0, -90.0, 180.0, 90.0];

/// PostGIS deste servidor tem suporte a MVT (protobuf-c em tempo de
/// compilação)? Builds conda-forge atuais saem **sem** protobuf-c —
/// `ST_AsMVT` existe mas falha em runtime (`Compiled without protobuf-c
/// support`). A imagem docker `postgis/postgis:16-3.4` tem suporte; o
/// probe evita falso-vermelho no harness local e documente o skip.
pub async fn mvt_available(pool: &PgPool) -> Result<bool> {
    let ok: bool = sqlx::query_scalar("SELECT postgis_full_version() LIKE '%LIBPROTOBUF%'")
        .fetch_one(pool)
        .await?;
    Ok(ok)
}

/// Parâmetros de um tile MVT.
#[derive(Debug, Clone, PartialEq)]
pub struct MvtTile {
    /// Zoom (0..=22).
    pub z: i32,
    /// Coluna X do tile.
    pub x: i32,
    /// Linha Y do tile (origem no canto NO, esquema XYZ).
    pub y: i32,
    /// Extensão da grade do tile em unidades do tile (padrão: 4096).
    pub extent: i32,
    /// Nome da camada vetorial DENTRO do tile (default: "features").
    pub layer_name: String,
    /// Buffer de clipping em unidades do tile (padrão: 256).
    pub buffer: i32,
}

impl MvtTile {
    pub fn new(z: i32, x: i32, y: i32) -> Self {
        Self {
            z,
            x,
            y,
            extent: 4096,
            layer_name: "features".into(),
            buffer: 256,
        }
    }

    /// Tolerância de simplificação em **graus**, proporcional ao zoom:
    /// largura do tile (360/2^z) ÷ extensão da grade. Zoom alto → tolerância
    /// minúscula; zoom baixo → simplificação agressiva
    /// (`ST_SimplifyPreserveTopology`).
    pub fn tolerance_deg(&self) -> f64 {
        let tile_width_deg = (WORLD_BOUNDS[2] - WORLD_BOUNDS[0]) / 2f64.powi(self.z);
        tile_width_deg / f64::from(self.extent.max(1))
    }
}

/// Gera o tile MVT (protocol buffer) da camada, com simplificação
/// proporcional ao zoom. `Ok(None)` = tile vazio (nenhuma feição no bbox).
///
/// Query única bind-only: `ST_TileEnvelope` com bounds geográficos (4674),
/// filtro por `&&` (GIST), `ST_SimplifyPreserveTopology` + `ST_AsMVTGeom`
/// (clip com buffer) e `ST_AsMVT` agregando o resultado.
pub async fn mvt_tile(pool: &PgPool, layer_id: &str, tile: &MvtTile) -> Result<Option<Vec<u8>>> {
    crate::error::check_id(layer_id)?;
    if !(0..=30).contains(&tile.z) {
        return Err(Error::Postgis(format!(
            "zoom inválido para tile MVT: {} (0..=30)",
            tile.z
        )));
    }
    let row: Option<Vec<u8>> = sqlx::query_scalar(
        "WITH env AS ( \
           SELECT ST_TileEnvelope($1::int4, $2::int4, $3::int4, \
                    bounds => ST_MakeEnvelope($4::double precision, $5::double precision, \
                                              $6::double precision, $7::double precision, \
                                              4674)) AS b \
         ), mvtgeom AS ( \
           SELECT ST_AsMVTGeom( \
                    ST_SimplifyPreserveTopology(f.geom, $8::double precision), \
                    env.b, $9::int4, $10::int4, true) AS geom, f.attrs \
           FROM features f CROSS JOIN env \
           WHERE f.layer_id = $11::uuid AND f.geom && env.b \
         ) \
         SELECT ST_AsMVT(mvtgeom.*, $12::text, $9::int4) \
         FROM mvtgeom",
    )
    .bind(tile.z)
    .bind(tile.x)
    .bind(tile.y)
    .bind(WORLD_BOUNDS[0])
    .bind(WORLD_BOUNDS[1])
    .bind(WORLD_BOUNDS[2])
    .bind(WORLD_BOUNDS[3])
    .bind(tile.tolerance_deg())
    .bind(tile.extent)
    .bind(tile.buffer)
    .bind(layer_id)
    .bind(&tile.layer_name)
    .fetch_one(pool)
    .await?;
    // ST_AsMVT de zero linhas → NULL → tile vazio.
    Ok(row.filter(|b| !b.is_empty()))
}

/// Garante que a extensão `postgis` existe no banco.
///
/// Falha com erro legível quando o servidor não tem o PostGIS instalado
/// (ex.: imagem vanilla do PostgreSQL) — a mensagem sugere o remedio.
pub async fn ensure_postgis(pool: &PgPool) -> Result<()> {
    let res = sqlx::query("CREATE EXTENSION IF NOT EXISTS postgis")
        .execute(pool)
        .await;
    if let Err(e) = res {
        return Err(Error::Postgis(format!(
            "não foi possível criar a extensão postgis ({e}); \
             instale o PostGIS 3.4 no servidor (ex.: conda-forge postgis=3.4, \
             apt postgresql-16-postgis-3, imagem postgis/postgis:16-3.4)"
        )));
    }
    Ok(())
}

/// PostGIS já está instalado e utilizável neste banco?
pub async fn is_postgis_ready(pool: &PgPool) -> Result<bool> {
    let ready: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'postgis')")
            .fetch_one(pool)
            .await?;
    Ok(ready)
}

/// Aplica as migrações sqlx do crate (`migrations/`) de forma idempotente.
///
/// Usa o `Migrator` em modo runtime (sem macro compile-time): o caminho é
/// resolvido a partir de `CARGO_MANIFEST_DIR`, então funciona de qualquer
/// working directory e não exige DATABASE_URL na compilação (risco R2).
pub async fn run_migrations(pool: &PgPool) -> Result<()> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migrator = sqlx::migrate::Migrator::new(dir).await?;
    migrator.run(pool).await?;
    Ok(())
}

/// Versões das migrações disponíveis em `migrations/` (crescente).
///
/// Lê o MESMO diretório que [`run_migrations`] aplica — é o lado "esperado"
/// da comparação feita pelo `ta-cli doctor` (`_sqlx_migrations` é o lado
/// aplicado).
pub async fn local_migration_versions() -> Result<Vec<i64>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migrator = sqlx::migrate::Migrator::new(dir).await?;
    Ok(migrator.iter().map(|m| m.version).collect())
}

/// Versões já aplicadas no banco (`_sqlx_migrations`), crescente.
///
/// `Ok(vec![])` = migrações nunca rodaram neste banco (tabela ausente), o que
/// o `ta-cli doctor` traduz em "rode `ta-cli migrate`" em vez de erro cru.
pub async fn applied_migration_versions(pool: &PgPool) -> Result<Vec<i64>> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations') IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if !exists {
        return Ok(Vec::new());
    }
    let versions: Vec<i64> = sqlx::query_scalar(
        "SELECT COALESCE(array_agg(version ORDER BY version), ARRAY[]::bigint[]) \
         FROM _sqlx_migrations",
    )
    .fetch_one(pool)
    .await?;
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srid_alvo_e_4674() {
        assert_eq!(TARGET_SRID, 4674);
    }

    /// As migrações versionadas do crate são 0001..=0004; o doctor depende de
    /// `local_migration_versions` enxergar exatamente esse conjunto (leitura de
    /// diretório, sem banco).
    #[tokio::test]
    async fn migracoes_locais_sao_lidas_do_diretorio() {
        let versions = local_migration_versions().await.expect("ler migrations/");
        assert_eq!(
            versions,
            vec![1, 2, 3, 4],
            "revise se novas migrações entraram"
        );
    }

    #[test]
    fn tolerancia_de_simplificaçao_e_proporcional_ao_zoom() {
        let z0 = MvtTile::new(0, 0, 0);
        let z10 = MvtTile::new(10, 300, 400);
        // Largura do tile em z=0: 360°/4096 unidades de grade.
        assert!((z0.tolerance_deg() - 360.0 / 4096.0).abs() < 1e-12);
        // z=10: 360/2^10/4096 ≈ 8.6e-5°.
        assert!((z10.tolerance_deg() - 360.0 / 1024.0 / 4096.0).abs() < 1e-12);
        // Monotônico decrescente no zoom.
        assert!(z10.tolerance_deg() < z0.tolerance_deg());
    }

    #[test]
    fn defaults_do_tile() {
        let t = MvtTile::new(4, 5, 6);
        assert_eq!(t.extent, 4096);
        assert_eq!(t.layer_name, "features");
        assert_eq!(t.buffer, 256);
    }
}
