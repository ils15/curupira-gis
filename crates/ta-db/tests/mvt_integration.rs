//! Integração MVT (`ST_AsMVT`) — **gated** por `TA_TEST_PG=1`.
//!
//! Sem a variável definida, cada teste imprime "skipped" e passa (mesma
//! disciplina de `pg_integration.rs`). Cluster: `scripts/test-pg.sh`.
//!
//! O tile MVT é um protobuf `Tile` (spec Mapbox v2):
//!
//! - campo 3 (`layers`, wire type 2) → primeiro byte `0x1A` + varint len;
//! - dentro do `Layer`: campo 1 (`name`) → `0x0A` + varint len + bytes;
//! - campo 15 (`version`, wire type 0) → `0x78` + valor `0x02`.
//!
//! Não existe "magic de arquivo" em MVT — o teste decodifica essa estrutura
//! à mão (sem deps extras de protobuf).

use std::sync::atomic::{AtomicU64, Ordering};

use geo_types::{LineString, Point, Polygon};
use sqlx::PgPool;
use ta_core::feature::Feature;
use ta_core::layer::{GeomType, Layer};
use ta_core::project::Project;
use ta_db::repo::{FeatureRepo, LayerRepo, PgFeatureRepo, PgLayerRepo, PgProjectRepo, ProjectRepo};
use ta_db::spatial::{MvtTile, is_postgis_ready, mvt_available, mvt_tile, run_migrations};
use tokio::sync::OnceCell;

/// DSN padrão = harness do `scripts/test-pg.sh`.
const DEFAULT_DSN: &str = "postgres://ta:ta@127.0.0.1:55432/ta";

static INIT: OnceCell<()> = OnceCell::const_new();

fn pg_enabled() -> bool {
    std::env::var("TA_TEST_PG").ok().as_deref() == Some("1")
}

async fn init_schema() {
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    let pool = sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness");
    if !is_postgis_ready(&pool).await.expect("pg_extension") {
        ta_db::spatial::ensure_postgis(&pool)
            .await
            .expect("PostGIS disponível");
    }
    run_migrations(&pool).await.expect("migrações aplicam");
    pool.close().await;
}

async fn test_pool() -> PgPool {
    INIT.get_or_init(init_schema).await;
    let dsn = std::env::var("TA_PG_DSN").unwrap_or_else(|_| DEFAULT_DSN.to_owned());
    sqlx::PgPool::connect(&dsn)
        .await
        .expect("conectar no PG do harness")
}

/// Pula com mensagem quando o PostGIS do harness não tem MVT
/// (protobuf-c ausente no build conda-forge; docker postgis:16-3.4 tem).
async fn mvt_ready_or_skip(pool: &PgPool) -> bool {
    match mvt_available(pool).await {
        Ok(true) => true,
        Ok(false) => {
            eprintln!(
                "[skip] PostGIS do harness compilou sem protobuf-c — ST_AsMVT \
                 indisponível (conda-forge); rode na imagem postgis/postgis:16-3.4"
            );
            false
        }
        Err(e) => {
            eprintln!("[skip] probe postgis_full_version falhou: {e}");
            false
        }
    }
}

fn unique_name(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{prefix}_{nanos:09}_{n:04}")
}

async fn make_layer(pool: &PgPool) -> (Project, Layer) {
    let projects = PgProjectRepo::new(pool.clone());
    let layers = PgLayerRepo::new(pool.clone());
    let project = projects
        .create(&Project {
            id: String::new(),
            name: unique_name("proj_mvt"),
            srid: 4674,
            created_at: time::OffsetDateTime::now_utc(),
        })
        .await
        .expect("criar projeto");
    let layer = layers
        .add(&Layer {
            id: String::new(),
            project_id: project.id.clone(),
            name: unique_name("lyr_mvt"),
            geom_type: GeomType::Polygon,
            srid: 4674,
            source_uri: "test://mvt".into(),
        })
        .await
        .expect("criar camada");
    (project, layer)
}

/// Decodifica o envelope do Tile protobuf: `0x1A` + varint + Layer.
/// Devolve os bytes da PRIMEIRA mensagem Layer.
fn first_layer_message(tile: &[u8]) -> Option<&[u8]> {
    if tile.first() != Some(&0x1A) {
        return None;
    }
    let mut len: u64 = 0;
    let mut shift = 0u32;
    let mut i = 1;
    while i < tile.len() {
        let b = tile[i];
        len |= u64::from(b & 0x7F) << shift;
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let rest = &tile[i..];
    if rest.len() < len as usize {
        return None;
    }
    Some(&rest[..len as usize])
}

/// Lê um varint protobuf a partir de `i`; devolve `(valor, próximo índice)`.
fn read_varint(buf: &[u8], mut i: usize) -> Option<(u64, usize)> {
    let mut v = 0u64;
    let mut shift = 0u32;
    while i < buf.len() {
        let b = buf[i];
        i += 1;
        v |= u64::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            return Some((v, i));
        }
        shift += 7;
        if shift > 63 {
            return None; // varint > 64 bits: stream malformado
        }
    }
    None
}

/// Avança `i` sobre o payload de um campo com o wire type dado.
/// Devolve `false` se o stream termina no meio do campo (malformado).
fn skip_field(buf: &[u8], i: &mut usize, wire: u64) -> bool {
    match wire {
        0 => match read_varint(buf, *i) {
            Some((_, next)) => {
                *i = next;
                true
            }
            None => false,
        },
        1 if buf.len() >= *i + 8 => {
            *i += 8;
            true
        }
        1 => false,
        2 => match read_varint(buf, *i) {
            Some((len, start)) if buf.len() >= start.saturating_add(len as usize) => {
                *i = start + len as usize;
                true
            }
            _ => false,
        },
        5 if buf.len() >= *i + 4 => {
            *i += 4;
            true
        }
        5 => false,
        _ => false, // wire 3/4 (groups) não existe no MVT
    }
}

/// Nome da camada dentro da mensagem Layer (campo 1) e versão (campo 15).
///
/// Protobuf NÃO garante ordem de serialização por número de campo: o
/// protobuf-c do PostGIS emite `name` (1) primeiro e `version` (15) por
/// último, após features/keys/values/extent. O walker então precisa PULAR
/// campos desconhecidos pelo wire type (causa raiz do vermelho do job
/// linux-pg na 2ª run de CI — a versão antiga fazia `break` no primeiro
/// tag não reconhecido e nunca chegava ao `version`).
fn decode_layer(layer: &[u8]) -> (Option<String>, Option<u64>) {
    let mut name = None;
    let mut version = None;
    let mut i = 0;
    while i < layer.len() {
        // Tag = varint (nº do campo << 3 | wire type).
        let (tag, next) = match read_varint(layer, i) {
            Some(x) => x,
            None => break,
        };
        i = next;
        let field = tag >> 3;
        let wire = tag & 0x07;
        match (field, wire) {
            (1, 2) => {
                // name (field 1, wire 2): varint len + bytes
                if let Some((len, start)) = read_varint(layer, i) {
                    let end = start.saturating_add(len as usize).min(layer.len());
                    name = Some(String::from_utf8_lossy(&layer[start..end]).into_owned());
                    i = start.saturating_add(len as usize);
                } else {
                    break;
                }
            }
            (15, 0) => {
                // version (field 15, wire 0): varint
                match read_varint(layer, i) {
                    Some((v, next)) => {
                        version = Some(v);
                        i = next;
                    }
                    None => break,
                }
            }
            _ => {
                // features (2) / keys (3) / values (4) / extent (5): fora do
                // escopo, mas precisam ser pulados corretamente para o walker
                // alcançar campos posteriores.
                if !skip_field(layer, &mut i, wire) {
                    break;
                }
            }
        }
    }
    (name, version)
}

/// Regressão do job `linux-pg` (run #2 de CI): o walker precisa pular
/// campos intermediários (extent/features) e achar `version` (15) mesmo
/// quando ele NÃO é o primeiro campo da mensagem Layer.
#[test]
fn decode_layer_pula_campos_e_acha_versao_apos_desconhecidos() {
    // Layer: name="abc" (1) · extent=255 (5, varint 2 bytes) ·
    // features (2, wire 2, payload qualquer) · version=2 (15) — ordem
    // propositalmente adversa à declaração do .proto.
    let bytes = [
        0x0A, 0x03, b'a', b'b', b'c', // name = "abc"
        0x28, 0xFF, 0x01, // extent = 255
        0x12, 0x02, 0xDE, 0xAD, // features (len 2)
        0x78, 0x02, // version = 2
    ];
    let (name, version) = decode_layer(&bytes);
    assert_eq!(name.as_deref(), Some("abc"));
    assert_eq!(version, Some(2));
}

#[tokio::test]
async fn mvt_tile_tem_estrutura_valida_e_versao_2() {
    if !pg_enabled() {
        eprintln!("[skip] TA_TEST_PG!=1 — integração MVT pulada");
        return;
    }
    let pool = test_pool().await;
    if !mvt_ready_or_skip(&pool).await {
        pool.close().await;
        return;
    }
    let (_, layer) = make_layer(&pool).await;
    let features = PgFeatureRepo::new(pool.clone());

    let poly = Polygon::new(
        LineString::from(vec![
            (-52.0, -12.0),
            (-51.0, -12.0),
            (-51.0, -11.0),
            (-52.0, -11.0),
            (-52.0, -12.0),
        ]),
        vec![],
    );
    let batch: Vec<Feature> = (0..50)
        .map(|i| {
            let dx = f64::from(i) * 0.001;
            Feature::new(
                geo_types::Geometry::from(translate(&poly, dx, dx)),
                serde_json::json!({"idx": i, "nome": format!("f_{i:03}")}),
            )
        })
        .collect();
    let inserted = features
        .bulk_insert(&layer.id, batch)
        .await
        .expect("bulk_insert");
    assert_eq!(inserted, 50);

    // Tile (3, 2, 4) = lon -90..-45, lat -22.5..0 — contém o lote inteiro
    // (lon -52..-51, lat -12..-11) SEM clipping. ST_TileEnvelope com bounds
    // geográficos divide o envelope LINEARMENTE (lat = 90 - y·180/2^z; sem
    // projeção de Mercator no eixo y). O tile original (6, 25, 38) caía no
    // Atlântico (lon -39.4..-33.8) → tile vazio → Ok(None) → panic — causa
    // raiz do vermelho do job linux-pg na 1ª run de CI (o teste nunca havia
    // rodado: o harness conda não tem protobuf-c e pulava os tiles reais).
    let tile = MvtTile {
        layer_name: "lote_teste".into(),
        ..MvtTile::new(3, 2, 4)
    };
    let bytes = mvt_tile(&pool, &layer.id, &tile)
        .await
        .expect("mvt_tile executa")
        .expect("tile não vazio sobre o lote");

    assert_eq!(bytes.first(), Some(&0x1A), "campo 3 (layers) esperado");
    let layer_msg = first_layer_message(&bytes).expect("envelope Layer válido");
    let (name, version) = decode_layer(layer_msg);
    assert_eq!(name.as_deref(), Some("lote_teste"));
    assert_eq!(version, Some(2), "MVT spec v2");
    pool.close().await;
}

fn translate(poly: &Polygon<f64>, dx: f64, dy: f64) -> Polygon<f64> {
    Polygon::new(
        LineString::new(
            poly.exterior()
                .0
                .iter()
                .map(|c| geo_types::Coord::from((c.x + dx, c.y + dy)))
                .collect(),
        ),
        vec![],
    )
}

#[tokio::test]
async fn mvt_zoom_alto_simplifica_mais_que_zoom_baixo() {
    if !pg_enabled() {
        eprintln!("[skip] TA_TEST_PG!=1 — integração MVT pulada");
        return;
    }
    let pool = test_pool().await;
    if !mvt_ready_or_skip(&pool).await {
        pool.close().await;
        return;
    }
    if !mvt_ready_or_skip(&pool).await {
        pool.close().await;
        return;
    }
    let (_, layer) = make_layer(&pool).await;
    let features = PgFeatureRepo::new(pool.clone());

    // Polígono "serrilhado" ESTRITAMENTE simples (a borda de fechamento
    // passa por baixo da serra, sem tocar nenhum vértice) — simplificação
    // em zoom baixo deve cortar vértices e encolher o tile.
    let mut ring = vec![(-52.0, -12.05)];
    for i in 0..=100 {
        let x = -52.0 + f64::from(i) * 0.01;
        let y = if i % 2 == 0 { -12.0 } else { -12.02 };
        ring.push((x, y));
    }
    ring.push((-51.0, -12.05));
    let geom = geo_types::Geometry::from(Polygon::new(LineString::from(ring), vec![]));
    features
        .bulk_insert(
            &layer.id,
            vec![Feature::new(geom, serde_json::json!({"id": 1}))],
        )
        .await
        .expect("bulk_insert");

    // (5, 11, 18) = lon -56.25..-45, lat -16.875..-11.25 — contém o
    // polígono inteiro (lon -52..-51, lat -12.05..-12.0) sem clipping. O
    // tile original (5, 25, 38) estourava o Y do mundo (y máx = 2^5-1 = 31)
    // → `ST_TileEnvelope: Invalid tile y value, 38` → panic no expect — a
    // outra causa raiz do job linux-pg na 1ª run.
    let z_baixo = MvtTile::new(5, 11, 18);
    // Tile z16 SOB o polígono (lon -51.998..-51.993, lat -12.038..-12.036 —
    // interior do anel, longe das bordas): 2048× menor que o z5, o clip
    // ST_AsMVTGeom reduz a ~1 quadrado (~5 comandos) enquanto o z5 carrega
    // os ~100 vértices da serra (tolerância z5 ≈ 0.0027° < dente 0.01°).
    let z_alto = MvtTile::new(16, 23302, 37150);
    let tile_baixo = mvt_tile(&pool, &layer.id, &z_baixo)
        .await
        .expect("tile z5")
        .expect("não vazio");
    // z16 mesmo lugar: tolerância 2048× menor; o tile pode nem existir
    // (tile menor que o polígono) — aceita None OU bytes menores.
    // z16 mesmo lugar: tolerância 2048× menor; o tile pode nem existir
    // (tile menor que o polígono) — aceita None OU bytes menores.
    if let Some(bytes) = mvt_tile(&pool, &layer.id, &z_alto).await.expect("tile z16") {
        assert!(
            bytes.len() < tile_baixo.len(),
            "esperava tile z16 ({}) <= z5 ({}) pela simplificação",
            bytes.len(),
            tile_baixo.len()
        );
    } // None = tile menor que a feição: vazio é válido
    pool.close().await;
}

#[tokio::test]
async fn mvt_area_vazia_e_none() {
    if !pg_enabled() {
        eprintln!("[skip] TA_TEST_PG!=1 — integração MVT pulada");
        return;
    }
    let pool = test_pool().await;
    if !mvt_ready_or_skip(&pool).await {
        pool.close().await;
        return;
    }
    let (_, layer) = make_layer(&pool).await;
    let features = PgFeatureRepo::new(pool.clone());
    features
        .bulk_insert(
            &layer.id,
            vec![Feature::new(
                geo_types::Geometry::from(Point::new(-51.9, -12.4)),
                serde_json::json!({"id": 1}),
            )],
        )
        .await
        .expect("bulk_insert");

    // Tile sobre o Pacífico: sem feição → None.
    let vazio = mvt_tile(&pool, &layer.id, &MvtTile::new(6, 62, 25))
        .await
        .expect("consulta executa");
    assert!(
        vazio.is_none(),
        "esperava tile vazio, veio {:?}",
        vazio.map(|b| b.len())
    );
    pool.close().await;
}

#[tokio::test]
async fn mvt_zoom_invalido_e_erro() {
    if !pg_enabled() {
        eprintln!("[skip] TA_TEST_PG!=1 — integração MVT pulada");
        return;
    }
    let pool = test_pool().await;
    let (_, layer) = make_layer(&pool).await;
    let err = mvt_tile(&pool, &layer.id, &MvtTile::new(99, 0, 0))
        .await
        .expect_err("zoom 99 deve falhar");
    assert!(err.to_string().contains("zoom"), "{err}");
    pool.close().await;
}
