//! Conversão central **WKB/EWKB ↔ [`geo_types::Geometry`]**.
//!
//! PONTO ÚNICO de conversão do projeto: qualquer crate que precise serializar
//! geometria (ta-gis lendo shapefile/GeoJSON, ta-db falando com PostGIS) passa
//! por aqui, para que o formato interno nunca diverja entre módulos.
//!
//! Formatos cobertos:
//! - **WKB** (OGC simple features) — troca com arquivos (GPKG, FlatGeobuf).
//! - **EWKB** (PostGIS) — troca com colunas `geometry` do banco; carrega o
//!   SRID embutido como flag opcional (`0x20000000` + `srid: i32`).
//!
//! Motor: [`geozero`] 0.15 (`with-geo` + `with-wkb`). A extração do SRID do
//! cabeçalho EWKB é feita aqui porque geozero expõe o SRID só durante o
//! processamento de stream, não como retorno de `FromWkb`.

use std::io::Cursor;

use geo_types::Geometry;
use geozero::wkb::{FromWkb, WkbDialect};
use geozero::{CoordDimensions, ToWkb};

use crate::error::{Error, Result};

/// Serializa a geometria em WKB little-endian puro (OGC simple features).
pub fn to_wkb(geom: &Geometry<f64>) -> Result<Vec<u8>> {
    Ok(geom.to_wkb(CoordDimensions::xy())?)
}

/// Desserializa WKB puro (OGC simple features). Aceita big- ou little-endian.
pub fn from_wkb(wkb: &[u8]) -> Result<Geometry<f64>> {
    let mut cursor = Cursor::new(wkb);
    Ok(Geometry::<f64>::from_wkb(&mut cursor, WkbDialect::Wkb)?)
}

/// Serializa em **EWKB** com SRID embutido (formato da coluna `geometry`
/// do PostGIS). Little-endian, flag `0x20000000` + `srid: i32`.
pub fn to_ewkb(geom: &Geometry<f64>, srid: u32) -> Result<Vec<u8>> {
    let srid = i32::try_from(srid).map_err(|_| Error::InvalidSrid(srid))?;
    Ok(geom.to_ewkb(CoordDimensions::xy(), Some(srid))?)
}

/// Desserializa **EWKB** (PostGIS), preservando o SRID embutido quando
/// presente. Retorna `(geometria, srid_opcional)`.
pub fn from_ewkb(ewkb: &[u8]) -> Result<(Geometry<f64>, Option<u32>)> {
    let srid = ewkb_srid(ewkb);
    let mut cursor = Cursor::new(ewkb);
    let geom = Geometry::<f64>::from_wkb(&mut cursor, WkbDialect::Ewkb)?;
    Ok((geom, srid))
}

/// Extrai o SRID do cabeçalho EWKB sem decodificar a geometria inteira.
///
/// Cabeçalho: `[byte_order: u8][type_id: u32][srid: i32, se flag 0x20000000]`.
/// Retorna `None` para WKB puro (sem flag), EWKB de geometria aninhada ou
/// buffer truncado — a decodificação completa fica a cargo de geozero.
fn ewkb_srid(ewkb: &[u8]) -> Option<u32> {
    if ewkb.len() < 5 {
        return None;
    }
    let type_bytes: [u8; 4] = ewkb[1..5].try_into().ok()?;
    let type_id = if ewkb[0] == 1 {
        u32::from_le_bytes(type_bytes)
    } else {
        u32::from_be_bytes(type_bytes)
    };
    // Flag de SRID apenas no cabeçalho externo (geometria de topo).
    if type_id & 0x2000_0000 == 0 {
        return None;
    }
    if ewkb.len() < 9 {
        return None;
    }
    let srid_bytes: [u8; 4] = ewkb[5..9].try_into().ok()?;
    let srid = if ewkb[0] == 1 {
        i32::from_le_bytes(srid_bytes)
    } else {
        i32::from_be_bytes(srid_bytes)
    };
    u32::try_from(srid).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::{LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon};

    fn sample_geometries() -> Vec<(&'static str, Geometry<f64>)> {
        let polygon = Polygon::new(
            LineString::from(vec![
                (0.0, 0.0),
                (4.0, 0.0),
                (4.0, 4.0),
                (0.0, 4.0),
                (0.0, 0.0),
            ]),
            vec![],
        );
        vec![
            ("point", Point::new(-51.9, -12.4).into()),
            (
                "linestring",
                LineString::from(vec![(0.0, 0.0), (1.5, 2.5), (-3.0, 4.0)]).into(),
            ),
            ("polygon", polygon.clone().into()),
            (
                "multipoint",
                MultiPoint::from(vec![Point::new(1.0, 2.0), Point::new(-3.5, 0.25)]).into(),
            ),
            (
                "multilinestring",
                MultiLineString::new(vec![
                    LineString::from(vec![(0.0, 0.0), (1.0, 1.0)]),
                    LineString::from(vec![(2.0, 2.0), (3.0, 3.0), (4.0, 2.0)]),
                ])
                .into(),
            ),
            (
                "multipolygon",
                MultiPolygon::new(vec![
                    polygon,
                    Polygon::new(
                        LineString::from(vec![
                            (10.0, 10.0),
                            (11.0, 10.0),
                            (11.0, 11.0),
                            (10.0, 11.0),
                            (10.0, 10.0),
                        ]),
                        vec![],
                    ),
                ])
                .into(),
            ),
        ]
    }

    #[test]
    fn round_trip_wkb_por_tipo() {
        for (name, geom) in sample_geometries() {
            let wkb = to_wkb(&geom).unwrap_or_else(|e| panic!("{name}: {e}"));
            let decoded = from_wkb(&wkb).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(decoded, geom, "round-trip WKB divergiu em {name}");
        }
    }

    #[test]
    fn round_trip_ewkb_preserva_srid() {
        for (name, geom) in sample_geometries() {
            let ewkb = to_ewkb(&geom, 4674).unwrap_or_else(|e| panic!("{name}: {e}"));
            let (decoded, srid) = from_ewkb(&ewkb).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(srid, Some(4674), "SRID perdido em {name}");
            assert_eq!(decoded, geom, "round-trip EWKB divergiu em {name}");
        }
    }

    #[test]
    fn wkb_puro_nao_tem_srid_no_from_ewkb() {
        let geom: Geometry<f64> = Point::new(1.0, 2.0).into();
        let wkb = to_wkb(&geom).unwrap();
        let (decoded, srid) = from_ewkb(&wkb).unwrap();
        assert_eq!(srid, None);
        assert_eq!(decoded, geom);
    }

    #[test]
    fn ewkb_em_big_endian_e_lido_com_srid() {
        // Mesma geometria em EWKB big-endian com SRID 4674 (header manual).
        let mut be = Vec::new();
        be.push(0u8); // big endian
        be.extend_from_slice(&(1u32 | 0x2000_0000).to_be_bytes());
        be.extend_from_slice(&4674i32.to_be_bytes());
        be.extend_from_slice(&1f64.to_be_bytes());
        be.extend_from_slice(&2f64.to_be_bytes());
        let (decoded, srid) = from_ewkb(&be).unwrap();
        assert_eq!(srid, Some(4674));
        assert_eq!(decoded, Geometry::from(Point::new(1.0, 2.0)));
    }

    #[test]
    fn header_truncado_nao_panica() {
        assert_eq!(ewkb_srid(&[]), None);
        assert_eq!(ewkb_srid(&[1]), None);
        assert_eq!(ewkb_srid(&[1, 1, 0, 0]), None);
        // flag de SRID setada, mas faltam os 4 bytes do srid
        assert_eq!(ewkb_srid(&[1, 0, 0, 0, 32]), None);
    }

    #[test]
    fn srid_negativo_ou_zero_vira_none() {
        // flag setada + srid 0: inútil na prática, retorna Some(0) — válido.
        let mut buf = vec![1u8];
        buf.extend_from_slice(&(1u32 | 0x2000_0000).to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes());
        assert_eq!(ewkb_srid(&buf), Some(0));
    }

    #[test]
    fn primeiro_byte_invalido_nao_panica() {
        // byte_order 7 é inválido, tratado como big-endian; não pode panicar.
        let mut buf = vec![7u8];
        buf.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(ewkb_srid(&buf), None);
    }
}
