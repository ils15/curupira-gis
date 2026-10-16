//! Erros da persistência.

use ta_core::layer::GeomType;

/// Erro da camada de dados.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Funcionalidade planejada, mas ainda não implementada (stub de Fase 0).
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),

    #[error(transparent)]
    Core(#[from] ta_core::Error),

    #[error(transparent)]
    Sql(#[from] sqlx::Error),

    /// Falha ao aplicar as migrações sqlx.
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),

    /// PostGIS indisponível/extensão ausente no servidor.
    #[error("PostGIS indisponível: {0}")]
    Postgis(String),

    /// Identificador esperado como UUID textual.
    #[error("id inválido (esperado UUID textual): {0:?}")]
    InvalidId(String),

    /// Tipo de geometria sem representação vetorial na persistência.
    #[error("geom_type sem representação vetorial na persistência: {0:?}")]
    UnsupportedGeomType(GeomType),

    /// O bulk COPY falhou e o fallback batched INSERT também — os dois erros
    /// são preservados para diagnóstico.
    #[error("COPY FROM STDIN falhou ({copy}) e o fallback batched INSERT também falhou")]
    CopyAndInsertFailed {
        copy: String,
        #[source]
        source: Box<Error>,
    },
}

/// Alias de resultado padrão da persistência.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Valida que o identificador é um UUID textual canônico (36 chars, hífens
/// nas posições 8/13/18/23, resto hex) antes de enviar ao banco — falha
/// rápida com erro legível em vez de "invalid input syntax for type uuid".
pub(crate) fn check_id(id: &str) -> Result<()> {
    let bytes = id.as_bytes();
    let hyphens = [8usize, 13, 18, 23];
    let ok = bytes.len() == 36
        && hyphens.iter().all(|&i| bytes[i] == b'-')
        && bytes
            .iter()
            .enumerate()
            .all(|(i, &b)| hyphens.contains(&i) || b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidId(id.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aceita_uuid_canonico() {
        assert!(check_id("0b9c4c86-1f2e-4a3b-9c8d-7e6f5a4b3c2d").is_ok());
    }

    #[test]
    fn rejeita_vazio_malformado_e_nao_hex() {
        assert!(check_id("").is_err());
        assert!(check_id("projeto-1").is_err());
        assert!(check_id("0b9c4c861f2e4a3b9c8d7e6f5a4b3c2d").is_err()); // sem hífens
        assert!(check_id("0b9c4c86-1f2e-4a3b-9c8d-7e6f5a4b3c2g").is_err()); // 'g' não é hex
    }
}
