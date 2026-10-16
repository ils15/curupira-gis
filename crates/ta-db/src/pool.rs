//! Pool de conexões PostgreSQL/PostGIS (sqlx + tokio).

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Configuração mínima do pool.
#[derive(Debug, Clone)]
pub struct DbConfig {
    /// DSN do banco, ex.: `postgres://ta:ta@localhost:5432/ta`
    pub url: String,
    /// Máximo de conexões no pool.
    pub max_connections: u32,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            url: "postgres://ta:ta@localhost:5432/ta".to_string(),
            max_connections: 8,
        }
    }
}

/// Abre o pool de conexões. Falha rápido se o banco não estiver acessível.
pub async fn connect(cfg: &DbConfig) -> sqlx::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .connect(&cfg.url)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_aponta_para_o_compose_local() {
        let cfg = DbConfig::default();
        assert!(cfg.url.contains("localhost:5432"));
        assert!(cfg.url.ends_with("/ta"));
        assert!(cfg.max_connections > 0);
    }
}
