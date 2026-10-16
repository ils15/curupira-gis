//! Utilitários de teste — localização dos fixtures versionados.

use std::path::{Path, PathBuf};

/// Diretório de fixtures: `<repo>/testdata` (env `TA_TESTDATA` sobrescreve).
///
/// Os fixtures são gerados por `testdata/gen_fixtures.py` (seed fixa 42);
/// num checkout recém-clonado eles podem não existir — os testes de arquivo
/// pulam com aviso nesse caso (economia de testes: nada de rede/build aqui).
pub(crate) fn testdata_dir() -> PathBuf {
    std::env::var_os("TA_TESTDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
            p.canonicalize().unwrap_or(p)
        })
}

/// Pula o teste com aviso quando os fixtures não foram gerados.
pub(crate) fn fixtures_presentes() -> bool {
    let ok = testdata_dir().join("manifest.json").exists();
    if !ok {
        eprintln!(
            "[skip] fixtures ausentes em {} — gere com `python3 testdata/gen_fixtures.py`",
            testdata_dir().display()
        );
    }
    ok
}
