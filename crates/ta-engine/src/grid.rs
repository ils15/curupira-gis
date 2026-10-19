//! Grade regular de células sobre uma AOI — o núcleo puro da Fase 1.
//!
//! # Contrato
//!
//! `generate_grid` recebe a geometria da AOI ([`MultiPolygon`]) e uma
//! [`GridSpec`] e devolve a lista **determinística** de células que pertencem à
//! AOI. A grade é **alinhada à origem** (`origin_x`/`origin_y`) e caminha em
//! passos de `resolution` **nas unidades do SRS `spec.srid`**: graus se o EPSG
//! for geográfico (ex.: 4674), metros se for projetado (ex.: 5880), ou o que o
//! EPSG definir (Decisão 4, retificada pelo usuário em 2026-10-18).
//!
//! O motor é **numericamente agnóstico ao SRID**: a geometria da AOI e a
//! [`GridSpec`] precisam estar **no mesmo** SRS, mas o motor não sabe (nem
//! precisa saber) qual é — ele só exige `srid > 0`. Reprojeção (de/para o SRS
//! de armazenamento EPSG:4674) é responsabilidade da fronteira de IO
//! (`ta-db`, PostGIS `ST_Transform`).
//!
//! # Regra de borda (clipping)
//!
//! Uma célula que **cruza** a borda da AOI é **mantida se o seu centro** cair
//! dentro da AOI e **descartada se o centro cair fora**. É uma regra explícita,
//! estável e independente de precisão de interseção: `centro ⊆ AOI ⇔ célula
//! incluída`. Consequência: células parcialmente dentro podem ser mantidas (com
//! a parte de fora), e a união das células pode exceder ligeiramente a AOI.
//!
//! # Convenção de fronteira (determinística)
//!
//! A pertinência usa o **intervalo semiaberto** em cada eixo: um ponto
//! exatamente sobre a fronteira da AOI é decidido pela regra de cruzamento
//! semiaberta (`y ∈ [y_edge_min, y_edge_max)`), o que na prática equivale a
//! `centro ∈ [x_min, x_max) × [y_min, y_max)` para envelopes alinhados aos
//! eixos:
//!
//! - aresta de **mínimo** (`x = x_min`/`y = y_min`) → **dentro**;
//! - aresta de **máximo** (`x = x_max`/`y = y_max`) → **fora**.
//!
//! O resultado **não é "indefinido"**: é a mesma convenção para toda execução,
//! coberta por teste (`fronteira_da_aoi_segue_intervalo_semiaberto`).
//!
//! # Grade vazia
//!
//! Uma AOI **degenerada**/vazia é rejeitada com [`GridError::DegenerateAoi`].
//! Uma AOI **válida** cuja grade não manteve nenhuma célula (resolução maior
//! que a AOI, origem desalinhada) é rejeitada com [`GridError::EmptyGrid`] em
//! vez de devolver `Ok(vec![])` silenciosamente.
//!
//! # Identificadores
//!
//! `cell_id = "<col>_<row>"` (ambos 0-based); `row_index`/`col_index` são
//! devolvidos junto para consumo direto. A ordem de iteração é
//! linha-crescente e, dentro da linha, coluna-crescente.

use std::cmp::Ordering;

use geo_types::{LineString, MultiPolygon, Point, Polygon};
use ta_core::cell::GridSpec;
use ta_core::error::{GridError, Result};

/// Teto absoluto de células de uma única grade.
///
/// Proteção de memória do motor puro: independentemente da política de negócio
/// (`projects.max_cells`), uma grade acima disso é rejeitada com
/// [`GridError::TooManyCells`] em vez de estourar a memória.
pub const MAX_CELLS: u64 = 5_000_000;

/// Uma célula produzida pelo motor (ainda não persistida).
#[derive(Debug, Clone, PartialEq)]
pub struct GridCell {
    /// Identificador determinístico `"<col>_<row>"` (0-based).
    pub cell_id: String,
    /// Índice de linha (0-based, alinhado à origem).
    pub row_index: i32,
    /// Índice de coluna (0-based, alinhado à origem).
    pub col_index: i32,
    /// Polígono da célula **no SRS da grade** (`spec.srid`), fechado, sem
    /// buracos.
    pub geom: Polygon<f64>,
}

/// Gera a grade regular recortada pela AOI.
///
/// Retorna as células na ordem linha→coluna, com coordenadas **no SRS da
/// `spec`**; erro tipado quando a especificação é inválida (`resolution ≤ 0`/não
/// finita, origem não finita, `srid ≤ 0`), a AOI é degenerada/vazia
/// ([`GridError::DegenerateAoi`]), nenhuma célula ficou dentro da AOI válida
/// ([`GridError::EmptyGrid`]), ou a grade excede [`MAX_CELLS`].
pub fn generate_grid(aoi: &MultiPolygon<f64>, spec: &GridSpec) -> Result<Vec<GridCell>> {
    let bounds = grid_bounds(aoi, spec)?;
    let needed = bounds.count();
    if needed > MAX_CELLS {
        return Err(GridError::TooManyCells {
            limit: MAX_CELLS,
            needed,
        }
        .into());
    }

    let res = spec.resolution;
    let mut out = Vec::with_capacity(usize::try_from(needed).unwrap_or(0));
    for row in bounds.row_min..bounds.row_max {
        for col in bounds.col_min..bounds.col_max {
            let x0 = spec.origin_x + col as f64 * res;
            let y0 = spec.origin_y + row as f64 * res;
            let cx = x0 + res / 2.0;
            let cy = y0 + res / 2.0;
            if !point_in_multi(aoi, Point::new(cx, cy)) {
                continue;
            }
            out.push(GridCell {
                cell_id: format!("{col}_{row}"),
                row_index: i32::try_from(row).map_err(|_| GridError::IndexOutOfRange(row))?,
                col_index: i32::try_from(col).map_err(|_| GridError::IndexOutOfRange(col))?,
                geom: cell_polygon(x0, y0, x0 + res, y0 + res),
            });
        }
    }
    // AOI válida (envelope com área) porém nenhum centro caiu dentro dela:
    // erro acionável em vez de `Ok(vec![])` silencioso.
    if out.is_empty() {
        return Err(GridError::EmptyGrid { resolution: res }.into());
    }
    Ok(out)
}

/// Número de células **candidatas** no envelope da AOI (antes do recorte pelo
/// centro). Serve para pré-validar limites (`projects.max_cells`) sem
/// materializar a grade.
pub fn candidate_count(aoi: &MultiPolygon<f64>, spec: &GridSpec) -> Result<u64> {
    Ok(grid_bounds(aoi, spec)?.count())
}

/// Intervalo de índices de coluna/linha que cobre o envelope da AOI
/// (`max` é **exclusivo**).
struct Bounds {
    col_min: i64,
    col_max: i64,
    row_min: i64,
    row_max: i64,
}

impl Bounds {
    fn count(&self) -> u64 {
        // A diferença é feita em `i128`: o cast `f64 as i64` satura em
        // `i64::MIN`/`i64::MAX` com coordenadas extremas, e a subtração em `i64`
        // estouraria (panic em debug). Em `i128` o resultado é sempre coerente.
        let cols = (i128::from(self.col_max) - i128::from(self.col_min)).unsigned_abs();
        let rows = (i128::from(self.row_max) - i128::from(self.row_min)).unsigned_abs();
        // Produto em `u128` (máx. `(2^64−1)²` < `2^128`, logo não estoura) e
        // saturação para `u64` — o teto [`MAX_CELLS`] decide o que fazer depois.
        let total = cols.saturating_mul(rows);
        u64::try_from(total).unwrap_or(u64::MAX)
    }
}

/// Valida a especificação e devolve o intervalo de índices que cobre a AOI.
fn grid_bounds(aoi: &MultiPolygon<f64>, spec: &GridSpec) -> Result<Bounds> {
    let res = spec.resolution;
    if !res.is_finite() {
        return Err(GridError::NonFiniteResolution(res).into());
    }
    if res <= 0.0 {
        return Err(GridError::NonPositiveResolution(res).into());
    }
    // O motor é agnóstico ao SRS, mas exige um EPSG positivo: ele define as
    // unidades da resolução/origem (graus vs metros). A existência do EPSG é
    // checada na fronteira de IO (`ta-db`).
    if spec.srid <= 0 {
        return Err(GridError::NonPositiveSrid(spec.srid).into());
    }
    if !spec.origin_x.is_finite() || !spec.origin_y.is_finite() {
        return Err(GridError::NonFiniteOrigin {
            x: spec.origin_x,
            y: spec.origin_y,
        }
        .into());
    }
    let (min_x, min_y, max_x, max_y) =
        aoi_bbox(aoi)
            .map(|[a, b, c, d]| (a, b, c, d))
            .ok_or(GridError::DegenerateAoi {
                min_x: 0.0,
                min_y: 0.0,
                max_x: 0.0,
                max_y: 0.0,
            })?;
    let spans_x = matches!(max_x.partial_cmp(&min_x), Some(Ordering::Greater));
    let spans_y = matches!(max_y.partial_cmp(&min_y), Some(Ordering::Greater));
    if !spans_x || !spans_y {
        return Err(GridError::DegenerateAoi {
            min_x,
            min_y,
            max_x,
            max_y,
        }
        .into());
    }
    Ok(Bounds {
        col_min: ((min_x - spec.origin_x) / res).floor() as i64,
        col_max: ((max_x - spec.origin_x) / res).ceil() as i64,
        row_min: ((min_y - spec.origin_y) / res).floor() as i64,
        row_max: ((max_y - spec.origin_y) / res).ceil() as i64,
    })
}

/// Envelope (`[min_x, min_y, max_x, max_y]`) das geometrias externas da AOI;
/// `None` quando não há nenhuma coordenada. Reusável por quem precisa alinhar
/// a origem da grade ao canto da AOI.
#[must_use]
pub fn aoi_bbox(aoi: &MultiPolygon<f64>) -> Option<[f64; 4]> {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for polygon in &aoi.0 {
        for coord in &polygon.exterior().0 {
            bounds = Some(match bounds {
                None => (coord.x, coord.y, coord.x, coord.y),
                Some((min_x, min_y, max_x, max_y)) => (
                    min_x.min(coord.x),
                    min_y.min(coord.y),
                    max_x.max(coord.x),
                    max_y.max(coord.y),
                ),
            });
        }
    }
    bounds.map(|(min_x, min_y, max_x, max_y)| [min_x, min_y, max_x, max_y])
}

/// Ponto dentro de pelo menos um polígono do multi-polígono.
fn point_in_multi(aoi: &MultiPolygon<f64>, point: Point<f64>) -> bool {
    aoi.0.iter().any(|polygon| point_in_polygon(polygon, point))
}

/// Ponto no polígono = dentro do anel externo e fora de todos os buracos.
fn point_in_polygon(polygon: &Polygon<f64>, point: Point<f64>) -> bool {
    if !point_in_ring(&polygon.exterior().0, point) {
        return false;
    }
    !polygon
        .interiors()
        .iter()
        .any(|hole| point_in_ring(&hole.0, point))
}

/// Teste ponto-em-anel por contagem de cruzamentos (even-odd / ray casting).
///
/// Robustez: ignora arestas horizontais (o produto dos sinais em `y` é nulo) e
/// trata NaN como "fora" (comparações `false`). Pontos exatamente sobre a
/// fronteira seguem a **convenção semiaberta documentada** no cabeçalho do
/// módulo: comparadores `>` em `y` e `<` em `x` atribuem as arestas de
/// **mínimo** ao interior e as de **máximo** ao exterior — resultado
/// determinístico, não "indefinido".
fn point_in_ring(ring: &[geo_types::Coord<f64>], point: Point<f64>) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let px = point.x();
    let py = point.y();
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let pi = ring[i];
        let pj = ring[j];
        if ((pi.y > py) != (pj.y > py)) && (px < (pj.x - pi.x) * (py - pi.y) / (pj.y - pi.y) + pi.x)
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Polígono fechado da célula (anel externo, sem buracos).
fn cell_polygon(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon<f64> {
    Polygon::new(
        LineString::from(vec![
            (min_x, min_y),
            (max_x, min_y),
            (max_x, max_y),
            (min_x, max_y),
            (min_x, min_y),
        ]),
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::LineString;
    use ta_core::error::Error;

    /// Retângulo `[min_x, min_y]–[max_x, max_y]` como AOI.
    fn rect(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> MultiPolygon<f64> {
        MultiPolygon::new(vec![Polygon::new(
            LineString::from(vec![
                (min_x, min_y),
                (max_x, min_y),
                (max_x, max_y),
                (min_x, max_y),
                (min_x, min_y),
            ]),
            vec![],
        )])
    }

    /// Spec em graus (EPSG:4674) na origem (0, 0) — caso geográfico.
    fn spec(res: f64) -> GridSpec {
        GridSpec::new(res, 0.0, 0.0, 4674)
    }

    #[test]
    fn grade_retangular_conta_e_ordena_celulas() {
        let cells = generate_grid(&rect(0.0, 0.0, 2.0, 2.0), &spec(1.0)).expect("grade");
        assert_eq!(cells.len(), 4);
        let ids: Vec<&str> = cells.iter().map(|c| c.cell_id.as_str()).collect();
        // Ordem linha→coluna; id = "<col>_<row>".
        assert_eq!(ids, vec!["0_0", "1_0", "0_1", "1_1"]);
        assert_eq!((cells[0].col_index, cells[0].row_index), (0, 0));
        assert_eq!((cells[3].col_index, cells[3].row_index), (1, 1));
    }

    #[test]
    fn borda_parcial_mantida_quando_o_centro_esta_dentro() {
        // AOI cobre até 1.6: as células de col/row 1 (centro em 1.5) ficam
        // parcialmente dentro → mantidas pela regra do centro.
        let cells = generate_grid(&rect(0.0, 0.0, 1.6, 1.6), &spec(1.0)).expect("grade");
        let ids: Vec<&str> = cells.iter().map(|c| c.cell_id.as_str()).collect();
        assert_eq!(ids, vec!["0_0", "1_0", "0_1", "1_1"]);
        // A célula "1_1" ultrapassa a AOI (vai até 2.0) — comportamento
        // esperado e documentado da regra do centro.
        assert!(cells[3].geom.exterior().0.iter().any(|c| c.x == 2.0));
    }

    #[test]
    fn celula_com_centro_fora_e_descartada() {
        // AOI cobre até 1.4: o centro de col/row 1 (1.5) cai fora → só a (0,0).
        let cells = generate_grid(&rect(0.0, 0.0, 1.4, 1.4), &spec(1.0)).expect("grade");
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].cell_id, "0_0");
    }

    #[test]
    fn buraco_da_aoi_exclui_celulas() {
        let mut aoi = rect(0.0, 0.0, 4.0, 4.0);
        // Buraco (1,1)–(3,3) remove as 4 células de centro dentro dele.
        aoi.0[0].interiors_push(LineString::from(vec![
            (1.0, 1.0),
            (3.0, 1.0),
            (3.0, 3.0),
            (1.0, 3.0),
            (1.0, 1.0),
        ]));
        let cells = generate_grid(&aoi, &spec(1.0)).expect("grade");
        assert_eq!(cells.len(), 12, "16 candidatas − 4 no buraco");
        assert!(!cells.iter().any(|c| c.cell_id == "1_1"));
        assert!(!cells.iter().any(|c| c.cell_id == "2_2"));
    }

    #[test]
    fn determinismo_mesma_entrada_mesma_ordem() {
        let aoi = rect(-52.205, -3.220, -52.142, -3.158);
        let spec = GridSpec::new(0.005, -52.205, -3.220, 4674);
        let a = generate_grid(&aoi, &spec).expect("1ª");
        let b = generate_grid(&aoi, &spec).expect("2ª");
        assert_eq!(a, b);
        assert!(!a.is_empty());
    }

    #[test]
    fn aoi_degenerada_e_erro_tipado() {
        // Multi-polígono vazio.
        let empty = MultiPolygon::new(Vec::new());
        assert!(matches!(
            generate_grid(&empty, &spec(1.0)),
            Err(Error::Grid(GridError::DegenerateAoi { .. }))
        ));
        // Polígono de área zero (todas as coordenadas colineares).
        let flat = MultiPolygon::new(vec![Polygon::new(
            LineString::from(vec![(0.0, 0.0), (1.0, 0.0), (0.0, 0.0)]),
            vec![],
        )]);
        assert!(matches!(
            generate_grid(&flat, &spec(1.0)),
            Err(Error::Grid(GridError::DegenerateAoi { .. }))
        ));
    }

    #[test]
    fn resolucao_invalida_e_erro_tipado() {
        let aoi = rect(0.0, 0.0, 2.0, 2.0);
        assert!(matches!(
            generate_grid(&aoi, &spec(0.0)),
            Err(Error::Grid(GridError::NonPositiveResolution(0.0)))
        ));
        assert!(matches!(
            generate_grid(&aoi, &spec(-1.0)),
            Err(Error::Grid(GridError::NonPositiveResolution(_)))
        ));
        assert!(matches!(
            generate_grid(&aoi, &spec(f64::NAN)),
            Err(Error::Grid(GridError::NonFiniteResolution(_)))
        ));
    }

    #[test]
    fn origem_nao_finita_e_erro_tipado() {
        let aoi = rect(0.0, 0.0, 2.0, 2.0);
        let bad = GridSpec::new(1.0, f64::INFINITY, 0.0, 4674);
        assert!(matches!(
            generate_grid(&aoi, &bad),
            Err(Error::Grid(GridError::NonFiniteOrigin { .. }))
        ));
    }

    #[test]
    fn srid_nao_positivo_e_erro_tipado() {
        let aoi = rect(0.0, 0.0, 2.0, 2.0);
        assert!(matches!(
            generate_grid(&aoi, &spec_zero_srid(0)),
            Err(Error::Grid(GridError::NonPositiveSrid(0)))
        ));
        assert!(matches!(
            generate_grid(&aoi, &spec_zero_srid(-1)),
            Err(Error::Grid(GridError::NonPositiveSrid(-1)))
        ));
        // `candidate_count` usa a MESMA validação.
        assert!(matches!(
            candidate_count(&aoi, &spec_zero_srid(0)),
            Err(Error::Grid(GridError::NonPositiveSrid(0)))
        ));
    }

    fn spec_zero_srid(srid: i32) -> GridSpec {
        GridSpec::new(1.0, 0.0, 0.0, srid)
    }

    #[test]
    fn srid_nao_muda_o_resultado_numerico() {
        // Propriedade pura: o motor é SRID-agnóstico. A MESMA spec numérica
        // (resolução/origem) em EPSGs diferentes (geográfico e projetado)
        // produz exatamente a mesma grade — nenhuma reprojeção aqui.
        let aoi = rect(0.0, 0.0, 2.0, 2.0);
        let geografico = GridSpec::new(1.0, 0.0, 0.0, 4674);
        let projetado = GridSpec::new(1.0, 0.0, 0.0, 5880);
        let utm = GridSpec::new(1.0, 0.0, 0.0, 31983);
        let base = generate_grid(&aoi, &geografico).expect("4674");
        assert_eq!(generate_grid(&aoi, &projetado).expect("5880"), base);
        assert_eq!(generate_grid(&aoi, &utm).expect("31983"), base);
    }

    #[test]
    fn grade_grande_demais_e_rejeitada_sem_alocar() {
        let world = rect(-180.0, -90.0, 180.0, 90.0);
        let err = generate_grid(&world, &spec(0.0001)).expect_err("deve exceder o teto");
        assert!(matches!(
            err,
            Error::Grid(GridError::TooManyCells {
                limit: MAX_CELLS,
                ..
            })
        ));
    }

    #[test]
    fn candidate_count_bate_com_a_grade_do_envelope() {
        let aoi = rect(0.0, 0.0, 2.0, 2.0);
        assert_eq!(candidate_count(&aoi, &spec(0.5)).expect("count"), 16);
    }

    #[test]
    fn coordenadas_extremas_nao_estouram_o_contador() {
        // Repro do blocker: `(col_max - col_min)` em `i64` estourava porque o
        // cast `f64 as i64` satura em `i64::MIN`/`i64::MAX`. O contrato do
        // módulo proíbe panic — deve devolver erro tratado.
        let extreme = rect(-1e308, -1e308, 1e308, 1e308);
        let spec = GridSpec::new(1e-300, 0.0, 0.0, 4674);
        let err = generate_grid(&extreme, &spec).expect_err("deve rejeitar, nunca panicar");
        assert!(matches!(err, Error::Grid(GridError::TooManyCells { .. })));
        // `candidate_count` usa a MESMA contagem — também não pode panicar.
        assert!(candidate_count(&extreme, &spec).expect("contagem") >= MAX_CELLS);
    }

    #[test]
    fn fronteira_da_aoi_segue_intervalo_semiaberto() {
        // Convenção semiaberta [x_min, x_max) × [y_min, y_max): as arestas de
        // mínimo pertencem ao interior; as de máximo ficam de fora. A caixa
        // (-0.5..0.5)² com passo 1 tem 4 centros; só o do canto de mínimo
        // (-0.5, -0.5) fica dentro.
        let cells = generate_grid(&rect(-0.5, -0.5, 0.5, 0.5), &spec(1.0)).expect("grade");
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].cell_id, "-1_-1");
    }

    #[test]
    fn grade_vazia_em_aoi_valida_e_erro_tipado() {
        // AOI válida (0.5 × 0.5) com célula maior que ela (passo 1): o único
        // centro cai exatamente no canto de MÁXIMO → excluído pela convenção
        // semiaberta → grade vazia. Deve ser erro tipado, não `Ok(vec![])`.
        let err = generate_grid(&rect(0.0, 0.0, 0.5, 0.5), &spec(1.0))
            .expect_err("AOI válida sem células deve errar");
        assert!(matches!(err, Error::Grid(GridError::EmptyGrid { .. })));
        // Resolução muito maior que a AOI → mesma distinção (AOI válida, célula
        // grosseira demais).
        let err = generate_grid(&rect(0.0, 0.0, 2.0, 2.0), &spec(100.0))
            .expect_err("resolução ≫ AOI deve errar");
        assert!(matches!(err, Error::Grid(GridError::EmptyGrid { .. })));
    }

    /// Anel da AOI de `testdata/monitoring/aoi.geojson` (EPSG:4674), replicado
    /// aqui para o teste unitário puro não depender de IO/`serde_json`.
    fn fixture_aoi_ring() -> MultiPolygon<f64> {
        MultiPolygon::new(vec![Polygon::new(
            LineString::from(vec![
                (-52.205, -3.215),
                (-52.160, -3.220),
                (-52.142, -3.192),
                (-52.150, -3.165),
                (-52.178, -3.158),
                (-52.202, -3.178),
                (-52.205, -3.215),
            ]),
            vec![],
        )])
    }

    #[test]
    fn grade_da_aoi_fixture_tem_118_celulas() {
        // Pine da paridade motor↔PostGIS sobre o fixture real: com resolução
        // 0.005° e origem no canto da AOI, 118 centros caem dentro do polígono
        // (169 candidatas no envelope). Se este número mudar, a regra de borda
        // ou o recorte mudou — investigue antes de atualizar.
        let aoi = fixture_aoi_ring();
        let spec = GridSpec::new(0.005, -52.205, -3.220, 4674);
        assert_eq!(candidate_count(&aoi, &spec).expect("candidatas"), 169);
        let cells = generate_grid(&aoi, &spec).expect("grade");
        assert_eq!(cells.len(), 118);

        // Envelope (x e y) da união das células — pega também min_y/max_y.
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for cell in &cells {
            for coord in &cell.geom.exterior().0 {
                min_x = min_x.min(coord.x);
                min_y = min_y.min(coord.y);
                max_x = max_x.max(coord.x);
                max_y = max_y.max(coord.y);
            }
        }
        assert!((min_x - (-52.205)).abs() < 1e-9, "min_x={min_x}");
        assert!((max_x - (-52.140)).abs() < 1e-9, "max_x={max_x}");
        assert!((min_y - (-3.220)).abs() < 1e-9, "min_y={min_y}");
        assert!((max_y - (-3.160)).abs() < 1e-9, "max_y={max_y}");
    }
}
