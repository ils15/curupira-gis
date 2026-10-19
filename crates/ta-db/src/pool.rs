//! Pool de conexões PostgreSQL/PostGIS (sqlx + tokio).

use std::time::Duration;

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
    connect_with_timeout(cfg, DEFAULT_ACQUIRE_TIMEOUT).await
}

/// Timeout padrão de aquisição de conexão (o mesmo default do sqlx).
pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// Abre o pool com `acquire_timeout` explícito.
///
/// O diagnóstico (`ta-cli doctor`) usa um timeout curto: esperar 30 s para
/// dizer "o banco não responde" é péssima usabilidade com o banco fora do ar.
pub async fn connect_with_timeout(cfg: &DbConfig, timeout: Duration) -> sqlx::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(cfg.max_connections.max(1))
        .acquire_timeout(timeout)
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
