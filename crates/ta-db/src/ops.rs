//! Processamento espacial PostGIS (v0.2 — menu Processamento da GUI).
//!
//! Cada operação cria uma **camada derivada nova** no projeto (a fonte
//! nunca é modificada). Tudo roda dentro de **uma única transação**
//! (`pool.begin()`): registra a camada, copia o resultado do operador
//! PostGIS (`INSERT … SELECT` bind-only) e recalcula o bbox antes do
//! `commit()` — qualquer erro SQL (ou resultado vazio) dispara rollback e
//! a camada derivada **nunca fica órfã** no banco. SRID canônico 4674 nas
//! duas pontas — o typmod da coluna rejeita qualquer desvio.
//!
//! Segurança (gate themis): toda query usa **bind** (`$n`) — a única string
//! interpolada é nenhuma; nome da camada vai como dado (`$`), identificador
//! nunca é montado a partir de input.
//!
//! Nota de escopo: as funções são assíncronas sobre `&PgPool` e pensadas
//! para o worker de banco da GUI (`ta-gui::db_worker`), não para a UI
//! diretamente.

use sqlx::{PgPool, Postgres, Transaction};

use ta_core::layer::{GeomType, Layer};

use crate::error::{Error, Result};
use crate::repo::PgLayerRepo;
use crate::spatial::TARGET_SRID;

/// Operador de processamento suportado pelo menu Processamento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcOp {
    /// `ST_Buffer(geom, dist)` — distância em graus (SRID geográfico).
    Buffer,
    /// `ST_Intersection(a, b)` por par de bbox (`&&`) das duas camadas.
    Intersect,
    /// `ST_Union(geom)` agregado de todas as feições da camada fonte.
    Union,
}

impl ProcOp {
    /// Marca registrada nos attrs da camada derivada (rastreabilidade).
    fn tag(self) -> &'static str {
        match self {
            ProcOp::Buffer => "buffer",
            ProcOp::Intersect => "intersection",
            ProcOp::Union => "union",
        }
    }
}

/// Normaliza erro do servidor em `Error::Postgis` (mensagem legível na UI).
fn map_db(e: sqlx::Error) -> Error {
    match e {
        sqlx::Error::Database(db) => Error::Postgis(db.message().to_owned()),
        other => other.into(),
    }
}

/// Cria a camada derivada registrada no projeto **dentro da transação** e
/// devolve o `Layer` (id já atribuído pelo banco, ainda não commitado).
///
/// `source_uri` aponta a origem (`postgis://<op>/<camada-fonte>`), o tipo
/// fica `geometry` (genérico do schema — buffer de ponto vira polígono,
/// interseção pode baixar de dimensão; o app renderiza pelo que vier).
async fn create_derived_layer(
    tx: &mut Transaction<'_, Postgres>,
    project_id: &str,
    name: &str,
    op: ProcOp,
    source: &str,
) -> Result<Layer> {
    let layer = Layer {
        id: String::new(),
        project_id: project_id.to_owned(),
        name: name.to_owned(),
        geom_type: GeomType::GeometryCollection,
        srid: TARGET_SRID,
        source_uri: format!("postgis://{}/{}", op.tag(), source),
    };
    PgLayerRepo::add_on(&mut **tx, &layer).await
}

/// Buffer: `ST_Buffer` de cada feição da fonte (distância em graus).
/// Devolve a camada derivada criada (bbox já recalculado).
pub async fn buffer(
    pool: &PgPool,
    project_id: &str,
    source_layer_id: &str,
    distance_deg: f64,
    name: &str,
) -> Result<Layer> {
    let mut tx = pool.begin().await.map_err(map_db)?;
    let derived =
        create_derived_layer(&mut tx, project_id, name, ProcOp::Buffer, source_layer_id).await?;
    let res = sqlx::query(
        "INSERT INTO features (layer_id, geom, attrs) \
         SELECT $1::uuid, ST_Multi(ST_Buffer(f.geom, $2)), \
                jsonb_build_object('op', 'buffer', 'distance_deg', $2, \
                                   'source', f.layer_id::text) \
         FROM features f WHERE f.layer_id = $3::uuid",
    )
    .bind(&derived.id)
    .bind(distance_deg)
    .bind(source_layer_id)
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    finish(tx, derived, res.rows_affected()).await
}

/// Interseção: pares de feições cujas bboxes se cruzam (`&&`), com
/// `ST_Intersection` computado **uma única vez** na CTE (reaproveitado no
/// teste de vazio e no INSERT) — pares vazios são descartados no SQL.
pub async fn intersection(
    pool: &PgPool,
    project_id: &str,
    layer_a: &str,
    layer_b: &str,
    name: &str,
) -> Result<Layer> {
    let mut tx = pool.begin().await.map_err(map_db)?;
    let derived = create_derived_layer(
        &mut tx,
        project_id,
        name,
        ProcOp::Intersect,
        &format!("{layer_a}|{layer_b}"),
    )
    .await?;
    let res = sqlx::query(
        "WITH pairs AS ( \
             SELECT a.layer_id AS src_a, b.layer_id AS src_b, \
                    ST_Intersection(a.geom, b.geom) AS inters \
             FROM features a JOIN features b ON a.geom && b.geom \
             WHERE a.layer_id = $2::uuid AND b.layer_id = $3::uuid \
         ) \
         INSERT INTO features (layer_id, geom, attrs) \
         SELECT $1::uuid, ST_Multi(inters), \
                jsonb_build_object('op', 'intersection', 'source_a', src_a::text, \
                                   'source_b', src_b::text) \
         FROM pairs WHERE NOT ST_IsEmpty(inters)",
    )
    .bind(&derived.id)
    .bind(layer_a)
    .bind(layer_b)
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    finish(tx, derived, res.rows_affected()).await
}

/// União: dissolve todas as feições da fonte em um único multi-geom.
///
/// `f.layer_id` no attrs é **agregado** para respeitar o SQL de grupo (sem
/// isso o PG rejeita com 42803 — coluna não agregada junto de `ST_Union`).
/// Agrega sobre texto (`min(f.layer_id::text)` — não existe `min(uuid)` no
/// PG); como o `WHERE` prende uma única camada fonte por chamada, o valor
/// registrado é exatamente a camada de origem.
pub async fn union(
    pool: &PgPool,
    project_id: &str,
    source_layer_id: &str,
    name: &str,
) -> Result<Layer> {
    let mut tx = pool.begin().await.map_err(map_db)?;
    let derived =
        create_derived_layer(&mut tx, project_id, name, ProcOp::Union, source_layer_id).await?;
    let res = sqlx::query(
        "INSERT INTO features (layer_id, geom, attrs) \
         SELECT $1::uuid, ST_Multi(ST_Union(f.geom)), \
                jsonb_build_object('op', 'union', 'source', min(f.layer_id::text)) \
         FROM features f WHERE f.layer_id = $2::uuid",
    )
    .bind(&derived.id)
    .bind(source_layer_id)
    .execute(&mut *tx)
    .await
    .map_err(map_db)?;
    finish(tx, derived, res.rows_affected()).await
}

/// Pós-operação comum, ainda **dentro da transação**: exige resultado não
/// vazio (camada derivada vazia seria lixo invisível) e recalcula o bbox
/// materializado antes do `commit()`. Com `inserted == 0` devolve erro
/// **sem commit** — o drop da transação faz rollback e desfaz o registro
/// da camada, que nunca vira órfã (não há DELETE manual para esquecer).
async fn finish(mut tx: Transaction<'_, Postgres>, derived: Layer, inserted: u64) -> Result<Layer> {
    if inserted == 0 {
        // Rollback automático: o INSERT da camada derivada é desfeito junto.
        return Err(Error::Postgis(
            "operação não produziu geometria (verifique se as camadas se \
             sobrepõem e se têm feições)"
                .into(),
        ));
    }
    PgLayerRepo::update_bbox_on(&mut *tx, &derived.id).await?;
    tx.commit().await.map_err(map_db)?;
    Ok(derived)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_tem_contrato() {
        assert_eq!(ProcOp::Buffer.tag(), "buffer");
        assert_eq!(ProcOp::Intersect.tag(), "intersection");
        assert_eq!(ProcOp::Union.tag(), "union");
        assert_eq!(TARGET_SRID, 4674);
    }
}
