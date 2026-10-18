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

    /// EPSG desconhecido: não existe em `spatial_ref_sys`.
    ///
    /// A grade aceita **qualquer** EPSG (Decisão 4, retificada em 2026-10-18);
    /// o valor precisa apenas existir na tabela de referência do PostGIS. A FK
    /// `cell_layers.srid → spatial_ref_sys(srid)` é a garantia no banco; esta
    /// variante dá a mensagem acionável na validação em código.
    #[error(
        "EPSG {0} desconhecido: não está em spatial_ref_sys; informe um código \
         EPSG válido (ex.: 4674 SIRGAS 2000 em graus, 5880 Policônica SIRGAS \
         2000 em metros)"
    )]
    UnknownSrid(i32),

    /// O SRS declarado das geometrias de entrada difere do SRS da camada.
    ///
    /// `cells` são geradas no SRS da grade (`cell_layers.srid`) e reprojetadas
    /// para 4674 no insert. Se o chamador mentir sobre o SRS de entrada, o
    /// `ST_Transform` produziria lixo **sem erro**; esta variante impede isso.
    #[error(
        "SRS de entrada EPSG:{input} ≠ srid da camada de células EPSG:{layer}: \
         as células precisam estar expressas no SRS em que a grade foi definida \
         (o mesmo de `cell_layers.srid`); reprojete-as antes de inserir"
    )]
    SridLayerMismatch { input: i32, layer: i32 },

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

    /// A classe da regra não pertence ao projeto do ruleset.
    ///
    /// Uma regra só pode decidir uma classe do **mesmo projeto**: cruzar
    /// projetos produziria decisões que a UI não consegue exibir e que a
    /// exportação não consegue resolver.
    #[error(
        "classe `{class_id}` pertence ao projeto {class_project}, não ao projeto \
         {ruleset_project} do ruleset; use uma classe do mesmo projeto"
    )]
    ClassProjectMismatch {
        class_id: String,
        class_project: String,
        ruleset_project: String,
    },

    /// O pai referenciado (ruleset/classe) não existe.
    #[error("{kind} `{id}` não existe")]
    MissingParent { kind: &'static str, id: String },

    /// A classe ainda é decidida por regras e não pode ser apagada.
    ///
    /// Com a FK `rules.class_id → classes(id) ON DELETE RESTRICT`
    /// (migração `0011_rule_class_restrict`), o banco recusa o delete; esta
    /// variante traduz a violação em contagem e caminho de saída — remover/
    /// reapontar as regras, desativar a classe, ou apagar as duas juntas de
    /// forma explícita (`class delete --force`, transação única).
    #[error(
        "classe `{class_id}` é usada por {count} regra(s); remova/reaponte as \
         regras (`rule remove`) ou desative a classe, ou repita com --force \
         para apagar as regras junto"
    )]
    ClassInUseByRules { class_id: String, count: i64 },

    /// Corrida de versão ao gravar um ruleset: a unicidade
    /// `UNIQUE(project_id, name, version)` foi violada mesmo com o lock de
    /// linhagem (`pg_advisory_xact_lock`) — não é duplicidade de nome, é
    /// colisão de versão restante (ex.: escritor fora do repositório). A
    /// saída é tentar de novo, não escolher outro nome.
    #[error("corrida de versão ao gravar ruleset `{name}`; tente novamente")]
    VersionRace { name: String },

    /// `parent_id` formaria um ciclo na hierarquia de classes (a cadeia de
    /// mães volta na própria classe, ou excede o limite de profundidade —
    /// ciclo provável). Inclui auto-pai.
    #[error(
        "ciclo na hierarquia de classes: `{class_id}` não pode ter `parent_id` \
         {parent_id} (a cadeia de mães volta nele); escolha uma mãe fora da \
         própria descendência"
    )]
    ClassParentCycle { class_id: String, parent_id: String },
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
