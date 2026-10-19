# Curupira GIS

[![ci](https://github.com/ils15/curupira-gis/actions/workflows/ci.yml/badge.svg)](https://github.com/ils15/curupira-gis/actions/workflows/ci.yml)
[![release](https://github.com/ils15/curupira-gis/actions/workflows/release.yml/badge.svg)](https://github.com/ils15/curupira-gis/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE-MIT)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE-APACHE)

> **Clean-room desktop GIS for Amazon forest monitoring.**
> SIG desktop clean-room para monitoramento florestal da Amazônia.

<!-- TODO(fase 4): screenshot — substituir por docs/screenshot-main.png após
     primeira run com display:
![Curupira GIS](docs/screenshot-main.png) -->

GIS desktop em Rust com escopo funcional **tipo TerraAmazon 7.3.2**
(projeto/banco, importação vetorial/raster, visualização, edição básica,
exportação), escrito **do zero** sobre stack moderna.

> **Clean-room**: reimplementação independente, sem código derivado de
> TerraAmazon/TerraLib. Referências apenas comportamentais/públicas.
> Ver [`NOTICE.md`](NOTICE.md) e [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Instalação

Binários de release (Windows zip com GDAL embutida; Linux tar.gz) em
[Releases](https://github.com/ils15/curupira-gis/releases) — builds marcados
como **draft** até revisão humana. Instruções completas por OS:
**[INSTALL.md](INSTALL.md)**.

## Quickstart (1 comando)

```sh
bash scripts/dev-up.sh          # banco + schema + projeto de demonstração
bash scripts/dev-up.sh --gui    # o mesmo e abre o desktop (precisa de display)
```

O `dev-up.sh` escolhe o banco sozinho — `docker compose up -d postgis` quando
há docker, cluster efêmero via conda (`scripts/test-pg.sh`) quando não há — e
depois roda `ta-cli doctor` → `ta-cli migrate` → `ta-cli seed-demo`,
**validando de verdade** (doctor precisa sair 0) antes de terminar. É
idempotente: rodar duas vezes não sobe banco duplicado nem duplica dados.

Na GUI: **Conectar** (a URL já vem preenchida via `TA_DATABASE_URL`) e
**Ctrl+O** para abrir o projeto `demo` — AOI, 20 pontos em 3 classes e a grade
provisória, prontos para visualizar.

Pré-requisitos: [Rust](https://rustup.rs) e GDAL
(`sudo apt install libgdal-dev` / `vcpkg install gdal:x64-windows` no Windows —
o binário de release já traz GDAL embutida). Docker é **opcional**. Detalhes:
[INSTALL.md](INSTALL.md).

### Quickstart manual (sem o script)

```sh
docker compose up -d postgis                          # ou scripts/test-pg.sh --no-tests
cargo run -p ta-cli -- migrate                        # schema (idempotente)
cargo run -p ta-cli -- doctor                         # exit 0 = ambiente ok
cargo run -p ta-cli -- seed-demo                      # projeto `demo`
cargo run -p ta-gui                                   # conecte e abra `demo`
```

DSN na ordem: `--dsn` > env `TA_PG_DSN` > `postgres://ta:ta@localhost:5432/ta`.

## Stack

| Camada | Escolha |
|---|---|
| Linguagem | Rust (stable, edition 2024) |
| GUI | egui + eframe, sobre a trait `MapView` (contrato migrável p/ MapLibre) |
| Persistência | PostgreSQL 16 + PostGIS 3.4 via `sqlx` (async) |
| SRID do projeto | **4674 — SIRGAS 2000** (datum geodésico, cartografia brasileira) |
| IO geoespacial | `gdal` (SHP/GPKG/FGB/GeoTIFF, link dinâmico), `geojson`, `flatgeobuf` (puro-Rust), `csv` (CSV-XY fino) |
| Raster | GDAL — ligação **dinâmica**, atrás da feature opcional `gdal` |
| Offline | GeoPackage via `rusqlite` (feature opt-in `offline-gpkg`) |
| Licença | MIT OR Apache-2.0 (dual) |

## Workspace

> **Por que os crates chamam `ta-*`?** Prefixo herdado do desenvolvimento
> inicial do projeto (código interno `terraamazon`), mantido para não quebrar
> o grafo de crates/benches já consolidado. A marca pública é **Curupira GIS**
> e o binário desktop entregue nas releases é `curupira-gis`.

```
crates/
├── ta-core   # domínio: Project, Layer, Feature, Style, WKB/EWKB, erros
├── ta-gis    # IO: reader/writer, normalização p/ 4674, derivados (COG/MVT/FlatGeobuf)
├── ta-db     # persistência: pool sqlx, repos async, espaciais; offline-gpkg (opt-in)
├── ta-gui    # desktop eframe + trait MapView (binário: curupira-gis)
└── ta-cli    # doctor, migrate, seed-demo, import-vector, import-raster, export, list-layers
```

## Licenças e clean-room

- Código: dupla licença **MIT OR Apache-2.0** (`LICENSE-MIT`, `LICENSE-APACHE`).
- **Barreira clean-room**: nenhum código/texto derivado do produto legado;
  comportamentos replicados apenas a partir de observação pública.
  Processo e regras em [`CONTRIBUTING.md`](CONTRIBUTING.md), aviso legal em
  [`NOTICE.md`](NOTICE.md).

## Banco de desenvolvimento (PostGIS 3.4 / PG16)

```sh
bash scripts/dev-up.sh        # forma recomendada: sobe o banco, migra, valida e semeia o demo
```

Manual, com docker:

```sh
cp .env.example .env          # opcional: TA_PG_USER / TA_PG_PASSWORD
docker compose up -d postgis
# conexão: postgres://ta:ta@localhost:5432/ta
```

Sem docker (host como o de referência, com sudo bloqueado), o
`scripts/test-pg.sh` provisiona um PG16 + PostGIS 3.4 efêmero via conda-forge
na porta 55432 (`--no-tests` sobe e deixa no ar, imprimindo o DSN e o comando
de parada).

O `docker-compose.yml` sobe `postgis/postgis:16-3.4` na porta 5432 com banco
`ta`. Migrações sqlx e o schema com `geometry(Geometry,4674)` + GIST + CHECK
de SRID são aplicadas idempotentemente pelo primeiro comando da CLI/GUI — ou
explicitamente por `ta-cli migrate`.

## Build & verificação

```sh
cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check        # opcional (licenças/advisories/bans) — ver deny.toml
```

Testes de integração (PostGIS) ficam **gated** por `TA_TEST_PG=1` e não
quebram `cargo test --workspace` sem banco: use
`scripts/test-pg.sh` (sobe o cluster, roda a suíte gated e derruba).

Baseline de performance do `ta-db` (`bulk_insert` 10k, `bbox_query`,
`list_with_ids`, `mvt_tile`) — números publicados em
[`benches/BASELINE.md`](benches/BASELINE.md):

```sh
TA_TEST_PG=1 TA_PG_DSN=postgres://ta:ta@127.0.0.1:55432/ta \
    cargo bench -p ta-db --bench ta_db_baseline
```

Features opcionais:

```sh
cargo build -p ta-gis --features gdal,geojson,csv,flatgeobuf   # IO completo (gdal = link dinâmico)
cargo build -p ta-gis --features wms                            # cliente WMS 1.3.0 + cache (reqwest async)
cargo build -p ta-db --features offline-gpkg
```

> `flatgeobuf` 6.0 (puro-Rust) escreve lotes homogêneos sem GDAL; lotes
> mistos caem no driver OGR "FlatGeobuf" (feature `gdal`).

## CLI

```sh
# ambiente: diagnóstico legível (Postgres/PostGIS/migrações/GDAL/escrita).
# exit 0 = tudo ok; exit != 0 = o relatório diz o que fazer
cargo run -p ta-cli -- doctor

# schema idempotente (extensão PostGIS + migrações) e relatório do que faltava
cargo run -p ta-cli -- migrate --to postgres://ta:ta@localhost:5432/ta

# projeto `demo` com dados SINTÉTICOS: AOI (testdata/monitoring/aoi.geojson),
# 20 pontos em 3 classes e a grade provisória. Idempotente.
cargo run -p ta-cli -- seed-demo

# import vetorial: detecta formato (SHP/GeoJSON/GPKG/FGB/CSV-XY), reprojeta
# para 4674, valida geometria, cria camada e faz bulk insert no PostGIS
cargo run -p ta-cli -- import-vector --input dados.shp --project minha-area
cargo run -p ta-cli -- import-vector --input pontos.csv --sep ';' \
    --lon-col lon --lat-col lat --srid 4326 --encoding latin1

# raster: registra metadados (dimensões, geotransform, bbox, CRS) —
# pixels NUNCA vão para o banco; opcionalmente deriva um COG ao lado
cargo run -p ta-cli -- import-raster --input imagem.tif --project minha-area \
    --cog imagem.cog.tif

# export de camada do banco (shp|geojson|gpkg|fgb|csv)
cargo run -p ta-cli -- export --layer <UUID> --format gpkg --output saida.gpkg

cargo run -p ta-cli -- list-layers --project minha-area
```

DSN: `--dsn` > env `TA_PG_DSN` > `postgres://ta:ta@localhost:5432/ta`.
O primeiro comando aplica as migrações idempotentemente. No desktop a conexão
usa o diálogo (ou `TA_DATABASE_URL` pré-preenchendo o campo URL).

### Fluxo de monitoramento (CLI)

Fase → AOI → grade → células bloqueadas. Os ids de camada saem de
`ta-cli list-layers`; os de AOI/grade aparecem na saída dos próprios comandos.

```sh
# 1) fase do fluxo (agrupa AOIs e grades)
ta-cli phase create --project demo --name "2026-10" --sort-order 1
ta-cli phase list --project demo

# 2) AOI = camada vetorial existente eleita como área de interesse
ta-cli list-layers --project demo            # pegue o UUID de `demo_aoi`
ta-cli aoi create --project demo --layer <UUID demo_aoi>

# 3) grade em GRAUS (4674, default): resolução 0.005° → 118 células no demo
ta-cli grid create --aoi-layer <UUID aoi_layers> --resolution 0.005 --name grade-4674

# 3b) a MESMA AOI em METROS (5880, Policônica): 500 m → 144 células
ta-cli grid create --aoi-layer <UUID aoi_layers> --resolution 500     --srid 5880 --name grade-5880

# 4) contagem, bloqueadas e extensão da grade
ta-cli grid info --cell-layer <UUID cell_layers>

# 5) bloquear/listar células (id "<col>_<row>", 0-based)
ta-cli cell block --cell-layer <UUID cell_layers> --cell-id 0_0 --reason "nuvem"
ta-cli cell list-blocked --cell-layer <UUID cell_layers>

# 6) área consolidada (SRS métrico de area_settings.srs_id, default EPSG:5880;
#    tabela em hectares; --draft acrescenta a área por classe; --json = contrato
#    do AreaReport para consumo programático)
ta-cli area report --cell-layer <UUID cell_layers> --draft <nome do rascunho> --json
```

**Unidades por SRS:** `--resolution`, `--origin-x` e `--origin-y` são expressos
no SRS de `--srid` — graus se geográfico (4674), metros se projetado (5880). As
células são **sempre persistidas em EPSG:4674** (armazenamento canônico); por
isso `grid info` rotula a origem no SRS da grade e a extensão em EPSG:4674.

## Releases

- Workflow `release.yml` dispara no push de tag `v*` e produz: binários
  Linux (`tar.gz` e `.deb`), Windows (`zip` portátil com GDAL/PROJ embutidos
  + instalador `CurupiraGIS-Setup-x64.exe`), macOS arm64
  (`CurupiraGIS-macos-arm64.zip`), checksums `SHA256SUMS` e SBOM CycloneDX
  por crate — **sempre como draft** para revisão humana antes de publicar.
- **macOS: build NÃO assinado.** No primeiro uso, clique com o botão direito
  no `CurupiraGIS.app` → **Abrir** (o Gatekeeper bloqueia apps sem notarização;
  sem esse passo o macOS só diz que o app "não pôde ser verificado").
- **Decisão de packaging (2026-10-16)**: workflow manual com matrix build em
  vez de [cargo-dist](https://opensource.axo.dev/cargo-dist/). Motivos:
  (a) o workflow do cargo-dist é gerado por `dist init`, que não pode ser
  produzido nem validado neste host; (b) matrix de 2 alvos não justifica a
  maquinaria de 20 alvos do cargo-dist; (c) workflow manual é 100%
  auditável e pinado por SHA. Reavaliar cargo-dist pós-alpha (alvo: Linux,
  macOS e Windows com instaladores).
- Targets de release: `x86_64-unknown-linux-gnu` (tar.gz + .deb),
  `x86_64-pc-windows-msvc` (zip portátil + instalador Inno Setup) e
  `aarch64-apple-darwin` (bundle .app, não assinado).

## WMS (feature `wms`, opt-in)

Cliente GetMap **1.3.0** assíncrono (`reqwest` rustls) com cache em disco:

- cache: `~/.cache/ta/wms/<sha256-da-query>.png`, **TTL configurável**
  (default 6 h) — arquivo fresco = zero rede;
- resposta validada (magic PNG + tamanho mínimo) e gravada atomicamente;
- BBOX segue a ordem de eixos do CRS informado (WMS 1.3.0: geográfico =
  lat,lon por spec — alguns servidores esperam lon,lat; o `GetMapRequest`
  carrega o BBOX na ordem que o servidor espera);

```rust
use ta_gis::wms::{GetMapRequest, WmsClient};

let client = WmsClient::new("https://servidor/geoserver/wms")?;
let path = client.get_map(&GetMapRequest {
    layers: "pop:setores".into(),
    bbox: [-10.0, -73.0, 2.0, -50.0], // ordem de eixos do CRS abaixo
    crs: "EPSG:4674".into(),
    width: 512,
    height: 512,
    ..Default::default()
}).await?; // -> caminho do PNG em cache
```

## Status

- **Fase 1 (fluxo de monitoramento)**: fase/AOI/grade/células bloqueadas no
  banco (`0005`–`0008`) e na CLI (`phase`/`aoi`/`grid`/`cell`) — ✅
  (implementada e revisada em 2026-10-18; aguardando commit)
- **Fase 0 (usabilidade + baseline)**: `scripts/dev-up.sh` (provisionamento em
  1 comando, com e sem docker), `ta-cli doctor` (exit 0/1 com remediação),
  `ta-cli seed-demo` (projeto `demo` idempotente, dados sintéticos em
  `testdata/monitoring/`), baseline `criterion` do `ta-db` publicado em
  `benches/BASELINE.md` e step não-gating de baseline no CI — ✅
- **Fase 4 (atual)**: packaging + release — rebrand Curupira GIS, binário
  `curupira-gis`, CI 3 jobs (Linux / Linux+PostGIS / Windows), release draft
  por tag com checksums + SBOM. Pendente: push, primeira run de CI, tag.
- Fase 0: scaffolding, licenciamento dual e barreira clean-room — ✅
- Fase 1: persistência PostGIS + migrações + `ta-cli migrate` — ✅
- Fase 2: IO geoespacial MVP + derivados (COG/MVT/FlatGeobuf) — ✅
- Fase 3: GUI egui sobre `MapView` + bench de decisão — ✅

Checkpoints de engenharia em `.pantheon/deepwork/terraamazon-cleanroom-rust/`.
