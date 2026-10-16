//! Mascaramento de DSN — credenciais nunca vazam em logs/erros do CLI.
//!
//! Ponto único de máscara para **todos** os lugares que imprimem DSN
//! (mensagem de erro de conexão em [`crate::open_pool`], relatório do
//! `migrate`). Suporta URI (`postgres://user:senha@host/db`) e a forma
//! keyword/value do GDAL (`... password=x`).

/// Mascara a senha do DSN para impressão em logs/relatórios.
#[must_use]
pub(crate) fn mask_password(dsn: &str) -> String {
    if let Some(rest) = dsn
        .strip_prefix("postgres://")
        .or_else(|| dsn.strip_prefix("postgresql://"))
    {
        let (scheme, _) = dsn.split_at(dsn.len() - rest.len());
        if let Some((userinfo, hostpart)) = rest.split_once('@') {
            if let Some((user, _pass)) = userinfo.split_once(':') {
                return format!("{scheme}{user}:***@{hostpart}");
            }
        }
    }
    // formato keyword=value
    if dsn.contains("password=") {
        return dsn
            .split_whitespace()
            .map(|kv| {
                if let Some(_i) = kv.strip_prefix("password=") {
                    "password=***"
                } else {
                    kv
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
    }
    dsn.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mascara_nao_vaza_senha() {
        let masked = mask_password("postgres://ta:segredo@127.0.0.1:55432/ta");
        assert!(!masked.contains("segredo"));
        assert!(masked.contains("***@127.0.0.1:55432/ta"));

        let masked2 = mask_password("PG:host=x dbname=y password=topsecret");
        assert!(!masked2.contains("topsecret"));
        assert!(masked2.contains("password=***"));
        // sem senha: intocado
        assert_eq!(
            mask_password("postgresql://localhost/db"),
            "postgresql://localhost/db"
        );
    }

    #[tokio::test]
    async fn erro_de_conexao_nao_ecoa_senha() {
        // Porta 99999 é inválida (>65535): sqlx falha no parse do DSN —
        // rápido e sem servidor; a mensagem de `open_pool` embute o DSN
        // mascarado exatamente como numa falha de conexão real.
        let err = crate::open_pool(Some("postgres://ta:senhafail@127.0.0.1:99999/ta"))
            .await
            .expect_err("DSN com porta inválida deve falhar");
        assert!(!err.contains("senhafail"), "erro ecoou a senha: {err}");
        assert!(
            err.contains("postgres://ta:***@127.0.0.1:99999/ta"),
            "erro sem máscara: {err}"
        );
    }
}
