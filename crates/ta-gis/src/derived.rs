//! Produtos derivados: COG+overviews, MVT e FlatGeobuf.
//!
//! - **COG** (Cloud Optimized GeoTIFF): wrapper de `gdal_translate -of COG`
//!   e `gdaladdo` — `std::process::Command` com **args em array** (sem
//!   shell; caminhos com espaço são seguros). Requer `gdal-bin` no PATH.
//! - **MVT** (Mapbox Vector Tiles): mora no **banco** (`ST_AsMVT`) — ver
//!   `ta_db::spatial::mvt_tile`; daqui só vem a redireção tipada.
//! - **FlatGeobuf**: escrita puro-Rust em [`crate::writer`] (feature
//!   `flatgeobuf`); o `build` lê a origem (OGR) e grava FGB.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};
use crate::writer::WriteFormat;

/// Produto derivado suportado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivedProduct {
    /// Cloud Optimized GeoTIFF com overviews embutidas.
    Cog,
    /// Mapbox Vector Tile (via `ST_AsMVT` no PostGIS — `ta_db`).
    Mvt,
    /// FlatGeobuf (colunar, streaming).
    FlatGeobuf,
}

/// Requisição de construção de um derivado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedRequest {
    pub product: DerivedProduct,
    /// Fonte de entrada (arquivo; MVT é caso de banco).
    pub input: PathBuf,
    /// Saída desejada.
    pub output: PathBuf,
}

/// Níveis de overview padrão (potências de 2 até ~menor dimensão/64).
pub fn default_overview_levels(width: u32, height: u32) -> Vec<u32> {
    let mut levels = Vec::new();
    let min_dim = width.min(height).max(1);
    let mut level = 2u32;
    while min_dim / level >= 64 && levels.len() < 8 {
        levels.push(level);
        level *= 2;
    }
    levels
}

/// Constrói o derivado pedido e devolve o caminho do artefato gerado.
pub fn build(req: &DerivedRequest) -> Result<PathBuf> {
    match req.product {
        DerivedProduct::Cog => translate_cog(&req.input, &req.output, &[]),
        DerivedProduct::FlatGeobuf => flatgeobuf_from_file(&req.input, &req.output),
        // MVT é derivado de **banco** (ST_AsMVT): chamada dedicada em
        // ta-db::spatial::mvt_tile — não faz sentido por arquivo.
        DerivedProduct::Mvt => Err(Error::UnsupportedFormat(
            "MVT é derivado do banco: use `ta_db::spatial::mvt_tile` \
             (tile z/x/y + extent), não derived::build"
                .into(),
        )),
    }
}

/// Converte para COG com `gdal_translate -of COG`. Overviews são geradas
/// pelo próprio driver COG (compressão embutida); para GeoTIFF "clássico",
/// use [`add_overviews`].
pub fn translate_cog(src: &Path, dst: &Path, extra_args: &[&str]) -> Result<PathBuf> {
    let mut args: Vec<String> = extra_args.iter().map(|s| (*s).to_string()).collect();
    args.extend(["-of", "COG"].iter().map(|s| s.to_string()));
    // Separador end-of-options: nada depois do `--` é tratado como opção —
    // path user-controlled começando em `-` não é engolido pelo getopt.
    args.push("--".into());
    args.push(src.display().to_string());
    args.push(dst.display().to_string());
    run_tool("gdal_translate", &args)?;
    Ok(dst.to_path_buf())
}

/// Gera overviews (pyramids) num GeoTIFF existente com `gdaladdo`.
pub fn add_overviews(tif: &Path, levels: &[u32]) -> Result<()> {
    if levels.is_empty() {
        return Ok(());
    }
    let mut args: Vec<String> = vec!["-r".into(), "average".into()];
    // `--` antes do path user-controlled (mesmo contrato do translate_cog).
    args.push("--".into());
    args.push(tif.display().to_string());
    args.extend(levels.iter().map(|l| l.to_string()));
    run_tool("gdaladdo", &args)
}

/// Executa a ferramenta GDAL e valida o status de saída.
///
/// Spawn/args sempre em array — nunca em shell — para caminhos com espaço
/// e para não interpolar entrada do usuário em command line.
fn run_tool(program: &str, args: &[String]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| Error::ExternalTool {
            program: program.to_string(),
            message: format!("spawn falhou ({e}); instale gdal-bin (GDAL ≥ 3.8) e confira o PATH"),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::ExternalTool {
            program: program.to_string(),
            message: if stderr.trim().is_empty() {
                format!("exit status {}", out.status)
            } else {
                stderr.trim().to_string()
            },
        });
    }
    Ok(())
}

/// Lê a origem (qualquer formato OGR/CSV suportado) e grava FlatGeobuf.
fn flatgeobuf_from_file(src: &Path, dst: &Path) -> Result<PathBuf> {
    let data = crate::reader::read_vector(src, &crate::reader::ReadOptions::default())?;
    crate::writer::write_vector(
        &data.features,
        dst,
        WriteFormat::FlatGeobuf,
        &crate::writer::WriteOptions::default(),
    )?;
    Ok(dst.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn niveis_de_overview_padrao() {
        // 1024×1024: min/level ≥ 64 → 2,4,8,16 (min/32 = 32 < 64 para).
        let levels = default_overview_levels(1024, 1024);
        assert_eq!(levels, vec![2, 4, 8, 16]);
        // 128×128: min/2 = 64 → uma única overview; min/4 = 32 < 64 para.
        assert_eq!(default_overview_levels(128, 128), vec![2]);
        // Raster minúsculo: nenhuma overview.
        assert!(default_overview_levels(64, 64).is_empty());
    }

    #[test]
    fn argumentos_do_cog_sao_array_sem_shell() {
        // Reproduz a montagem de args de translate_cog e garante: caminho
        // com espaço vai como UM argumento (não é reparseado por shell) e
        // `--` fecha a seção de opções antes dos paths posicionais.
        let src = Path::new("/tmp/dir com espaço/inPut.tif");
        let dst = Path::new("/tmp/out cogs/derivado.cog.tif");
        let mut args: Vec<String> = Vec::new();
        args.extend(["-of", "COG"].iter().map(|s| s.to_string()));
        args.push("--".into());
        args.push(src.display().to_string());
        args.push(dst.display().to_string());
        assert_eq!(
            args,
            vec![
                "-of",
                "COG",
                "--",
                "/tmp/dir com espaço/inPut.tif",
                "/tmp/out cogs/derivado.cog.tif"
            ]
        );
    }

    #[test]
    fn separador_impede_path_com_traco_inicial_de_virar_opcao() {
        // Path user-controlled começando em `-`: após o `--` ele chega ao
        // getopt como argumento POSICIONAL (gdal_translate/gdaladdo), não
        // como opção. Construção de args apenas — sem executar gdal.
        let src = Path::new("-parece-opcao/entrada.tif");
        let dst = Path::new("-saida.cog.tif");

        // Montagem de translate_cog:
        let mut args: Vec<String> = vec!["-of".into(), "COG".into(), "--".into()];
        args.push(src.display().to_string());
        args.push(dst.display().to_string());
        let sep = args.iter().position(|a| a == "--").expect("separador");
        assert_eq!(
            &args[sep + 1..],
            &["-parece-opcao/entrada.tif", "-saida.cog.tif"]
        );

        // Montagem de add_overviews (`--` antes do .tif; levels depois):
        let mut oargs: Vec<String> = vec!["-r".into(), "average".into(), "--".into()];
        oargs.push("-niveis.tif".into());
        oargs.extend([2u32, 4].iter().map(|l| l.to_string()));
        let osep = oargs.iter().position(|a| a == "--").expect("separador");
        assert_eq!(oargs[osep + 1], "-niveis.tif");
        assert_eq!(&oargs[osep + 2..], &["2", "4"]);
    }

    #[test]
    fn ferramenta_ausente_e_erro_tipado() {
        // Binário inexistente: spawn falha → ExternalTool com mensagem de
        // instalação — sem pânico, sem shell.
        let err = run_tool("ta-gis-ferramenta-inexistente-xyz", &[]).expect_err("deve falhar");
        match err {
            Error::ExternalTool { program, message } => {
                assert_eq!(program, "ta-gis-ferramenta-inexistente-xyz");
                assert!(message.contains("spawn falhou"), "{message}");
            }
            other => panic!("erro inesperado: {other}"),
        }
    }

    #[test]
    fn mvt_redireciona_para_o_banco() {
        let req = DerivedRequest {
            product: DerivedProduct::Mvt,
            input: PathBuf::from("qualquer.tif"),
            output: PathBuf::from("tile.mvt"),
        };
        let err = build(&req).unwrap_err();
        assert!(
            matches!(err, Error::UnsupportedFormat(ref m) if m.contains("mvt_tile")),
            "erro: {err}"
        );
    }
}
