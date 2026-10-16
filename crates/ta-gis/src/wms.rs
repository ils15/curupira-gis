//! Cliente WMS 1.3.0 com cache em disco (feature `wms`, default **off**).
//!
//! - `reqwest` **async** (nunca blocking) — na CLI chama direto dentro do
//!   runtime tokio; em contexto síncrono, `tokio::runtime::Handle::block_on`.
//! - GetMap **1.3.0** com PNG; a resposta é validada (magic PNG + não vazia)
//!   antes de entrar no cache.
//! - Cache: `~/.cache/ta/wms/<sha256-da-query>.png`, com TTL configurável.
//!   Arquivo fresco (idade < TTL) → zero rede; vencido → revalida.
//!
//! Nota de axis order: no WMS 1.3.0 o BBOX segue a ordem de eixos do CRS
//! informado (para EPSG:4674 — geográfico — a ordem oficial é lat,lon;
//! alguns servidores esperam lon,lat). [`GetMapRequest`] carrega o BBOX na
//! ordem que o servidor espera — quem monta o request decide e documenta.

use std::path::{Path, PathBuf};
use std::time::Duration;

use percent_encoding::{AsciiSet, CONTROLS, percent_encode};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Conjunto de escape para valores da query GetMap: tudo que não é
/// unreserved (`ALPHA DIGIT -._~`) é percent-encodado, **exceto** `:` e
/// `,` — `:` é parte canônica de CRS (`EPSG:4674`, URN) e `,` separa
/// múltiplas camadas (contrato de [`GetMapRequest::layers`]); nenhum dos
/// dois colide com a separação `chave=valor&` da query. Isso barra
/// injeção de parâmetro via `&`/`=`/`#`/`%`/espaço em valores
/// user-controlled (`layers`, `crs`, `format`).
const QUERY_VALUE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'!')
    .add(b'"')
    .add(b'#')
    .add(b'$')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b'/')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Percent-encoda um valor user-controlled da query (bytes não-ASCII
/// sempre escapados).
fn encode_value(value: &str) -> String {
    percent_encode(value.as_bytes(), QUERY_VALUE).to_string()
}

/// Requisição GetMap 1.3.0.
#[derive(Debug, Clone, PartialEq)]
pub struct GetMapRequest {
    /// Nome(s) da(s) camada(s) no servidor (separado por vírgula).
    pub layers: String,
    /// BBOX na ordem de eixos do `crs` (ver nota de axis order do módulo).
    pub bbox: [f64; 4],
    /// CRS do BBOX (ex.: `EPSG:4674`).
    pub crs: String,
    /// Largura da imagem em pixels.
    pub width: u32,
    /// Altura da imagem em pixels.
    pub height: u32,
    /// Formato de saída (padrão: `image/png`).
    pub format: String,
}

impl Default for GetMapRequest {
    fn default() -> Self {
        Self {
            layers: String::new(),
            bbox: [0.0; 4],
            crs: "EPSG:4674".into(),
            width: 256,
            height: 256,
            format: "image/png".into(),
        }
    }
}

impl GetMapRequest {
    /// Query string canônica do GetMap 1.3.0 (ordem fixa — base do hash).
    ///
    /// Valores user-controlled (`layers`, `crs`, `format`) são
    /// percent-encodados antes da interpolação — um `&`/`=` dentro de um
    /// valor não vira parâmetro extra.
    pub fn query_string(&self) -> String {
        format!(
            "SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&LAYERS={layers}&\
             STYLES=&CRS={crs}&BBOX={minx},{miny},{maxx},{maxy}&\
             WIDTH={width}&HEIGHT={height}&FORMAT={format}&TRANSPARENT=true",
            layers = encode_value(&self.layers),
            crs = encode_value(&self.crs),
            minx = self.bbox[0],
            miny = self.bbox[1],
            maxx = self.bbox[2],
            maxy = self.bbox[3],
            width = self.width,
            height = self.height,
            format = encode_value(&self.format),
        )
    }
}

/// Cliente WMS com cache em disco e TTL.
pub struct WmsClient {
    /// Endpoint base do servidor (ex.: `https://servidor/geoserver/wms`).
    endpoint: String,
    http: reqwest::Client,
    cache_dir: PathBuf,
    ttl: Duration,
}

/// Diretório de cache padrão: `$XDG_CACHE_HOME|~/.cache/ta/wms`.
pub fn default_cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".cache")
        });
    base.join("ta").join("wms")
}

impl WmsClient {
    /// Cliente novo com defaults de cache (`~/.cache/ta/wms`, TTL 6 h).
    pub fn new(endpoint: &str) -> Result<Self> {
        Self::with_cache(endpoint, default_cache_dir(), Duration::from_secs(6 * 3600))
    }

    /// Cliente com cache/TTL explícitos.
    pub fn with_cache(endpoint: &str, cache_dir: PathBuf, ttl: Duration) -> Result<Self> {
        if endpoint.trim().is_empty() {
            return Err(Error::Wms("endpoint vazio".into()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("ta-cleanroom-wms/0.1")
            .build()
            .map_err(|e| Error::Wms(format!("cliente HTTP: {e}")))?;
        Ok(Self {
            endpoint: endpoint.trim_end_matches(['?', '&']).to_string(),
            http,
            cache_dir,
            ttl,
        })
    }

    /// Caminho do cache para a query (sem tocar na rede).
    pub fn cache_path(&self, query: &str) -> PathBuf {
        cache_path_for(&self.cache_dir, query)
    }

    /// GetMap com cache: arquivo fresco (TTL) é reutilizado; caso contrário,
    /// baixa, valida PNG e grava. Devolve o caminho do PNG em disco.
    pub async fn get_map(&self, req: &GetMapRequest) -> Result<PathBuf> {
        let query = req.query_string();
        let path = self.cache_path(&query);
        if is_fresh(&path, self.ttl) {
            return Ok(path);
        }
        let url = format!("{}?{}", self.endpoint, query);
        let bytes = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::Wms(format!("GET {url}: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Wms(format!("GET {url}: {e}")))?
            .bytes()
            .await
            .map_err(|e| Error::Wms(format!("GET {url}: corpo ilegível: {e}")))?;

        validate_png(&bytes)?;
        store_atomic(&path, &bytes)?;
        Ok(path)
    }
}

/// Caminho de cache determinístico: sha256 da query → `<hash>.png`.
pub fn cache_path_for(dir: &Path, query: &str) -> PathBuf {
    let hash = Sha256::digest(query.as_bytes());
    let mut hex = String::with_capacity(64);
    for b in hash {
        hex.push_str(&format!("{b:02x}"));
    }
    dir.join(format!("{hex}.png"))
}

/// Arquivo existe e idade < TTL?
pub fn is_fresh(path: &Path, ttl: Duration) -> bool {
    match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(modified) => match modified.elapsed() {
            Ok(age) => age < ttl,
            // Relógio no futuro (distorção): tratar como fresco.
            Err(_) => true,
        },
        Err(_) => false,
    }
}

/// Valida assinatura PNG: `\x89PNG\r\n\x1a\n` + tamanho mínimo plausível.
pub fn validate_png(bytes: &[u8]) -> Result<()> {
    const MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 100 {
        return Err(Error::Wms(format!(
            "resposta muito pequena para ser PNG útil ({} bytes); \
             servidor retornou erro? Confira LAYERS/BBOX/CRS",
            bytes.len()
        )));
    }
    if bytes[..8] != MAGIC {
        return Err(Error::Wms(
            "resposta não é PNG (magic inválido); servidor pode ter \
             devolvido XML de erro — confira os parâmetros do GetMap"
                .into(),
        ));
    }
    Ok(())
}

/// Gravação atômica: escreve `<path>.tmp-<pid>` e renomeia — um request
/// concorrente nunca observa arquivo pela metade.
pub(crate) fn store_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::Wms(format!(
                "criar diretório de cache {}: {e}",
                parent.display()
            ))
        })?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)
        .map_err(|e| Error::Wms(format!("escrever {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::Wms(format!("renomear {}: {e}", tmp.display())))?;
    Ok(())
}

/// Utilitário de expiração manual (TTL muito longo / teste).
pub fn invalidate(dir: &Path) -> Result<usize> {
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().extension().is_some_and(|e| e == "png")
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let p = std::env::temp_dir().join(format!("ta-gis-wms-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn query_130_canonica() {
        let req = GetMapRequest {
            layers: "pop:Setores".into(),
            bbox: [-10.0, -73.0, 2.0, -50.0],
            crs: "EPSG:4674".into(),
            width: 512,
            height: 512,
            format: "image/png".into(),
        };
        let q = req.query_string();
        assert!(q.starts_with("SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap"));
        assert!(q.contains("CRS=EPSG:4674"));
        assert!(q.contains("BBOX=-10,-73,2,-50"));
        assert!(q.contains("WIDTH=512"));
    }

    #[test]
    fn query_escapa_valores_user_controlled() {
        // Valor de layer com `&` e `=` fica encoded: um único par
        // LAYERS=..., sem injeção de parâmetro extra na query.
        let req = GetMapRequest {
            layers: "pop:Setores&REQUEST=GetCapabilities".into(),
            ..Default::default()
        };
        let q = req.query_string();
        assert!(
            q.contains("LAYERS=pop:Setores%26REQUEST%3DGetCapabilities"),
            "query: {q}"
        );
        let pairs: Vec<&str> = q.split('&').collect();
        assert_eq!(
            pairs.iter().filter(|p| p.starts_with("LAYERS=")).count(),
            1,
            "um só par LAYERS: {q}"
        );
        assert!(!pairs.contains(&"REQUEST=GetCapabilities"), "query: {q}");
        // `:` e `,` permanecem intactos (CRS canônico e multi-layer).
        assert!(q.contains("CRS=EPSG:4674"), "query: {q}");
        let multi = GetMapRequest {
            layers: "a:B,c:D".into(),
            ..Default::default()
        };
        assert!(multi.query_string().contains("LAYERS=a:B,c:D"));
    }

    #[test]
    fn hash_do_cache_e_deterministico() {
        let dir = temp_dir("hash");
        let p1 = cache_path_for(&dir, "SERVICE=WMS&VERSION=1.3.0&BBOX=1");
        let p2 = cache_path_for(&dir, "SERVICE=WMS&VERSION=1.3.0&BBOX=1");
        let p3 = cache_path_for(&dir, "SERVICE=WMS&VERSION=1.3.0&BBOX=2");
        assert_eq!(p1, p2);
        assert_ne!(p1, p3);
        assert_eq!(p1.extension().and_then(|e| e.to_str()), Some("png"));
        // 64 hex chars = sha256.
        let stem = p1.file_stem().unwrap().to_str().unwrap();
        assert_eq!(stem.len(), 64);
        assert!(stem.bytes().all(|b| b.is_ascii_hexdigit()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ttl_respeita_idade_do_arquivo() {
        let dir = temp_dir("ttl");
        let p = dir.join("x.png");
        std::fs::write(&p, b"dados").unwrap();
        assert!(is_fresh(&p, Duration::from_secs(3600)));
        assert!(!is_fresh(&p, Duration::ZERO));
        assert!(!is_fresh(
            &dir.join("inexistente.png"),
            Duration::from_secs(3600)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validacao_png_aceita_e_rejeita() {
        let mut png = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend(std::iter::repeat_n(0u8, 200));
        assert!(validate_png(&png).is_ok());
        // XML de erro do servidor:
        let xml = br#"<ServiceExceptionReport>..</ServiceExceptionReport>"#;
        assert!(validate_png(xml).is_err());
        // PNG "vazio":
        assert!(validate_png(&png[..8]).is_err());
    }

    #[test]
    fn cliente_grava_cache_atomicamente_sem_rede() {
        // Grava um PNG fake via store_atomic e valida leitura "fresca".
        let dir = temp_dir("store");
        let client = WmsClient::with_cache(
            "http://wms.invalido/wms",
            dir.clone(),
            Duration::from_secs(60),
        )
        .unwrap();
        let query = "SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap";
        let path = client.cache_path(query);
        let mut png = vec![0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend(std::iter::repeat_n(1u8, 128));
        store_atomic(&path, &png).unwrap();
        assert!(path.exists());
        assert!(is_fresh(&path, Duration::from_secs(60)));
        assert_eq!(std::fs::read(&path).unwrap(), png);
        // tmp não vaza:
        assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| {
            !e.path()
                .extension()
                .is_some_and(|x| x.to_string_lossy().starts_with("tmp"))
        }));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
