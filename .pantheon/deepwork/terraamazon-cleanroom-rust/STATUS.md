# STATUS — terraamazon-cleanroom-rust

- **Slug:** `terraamazon-cleanroom-rust`
- **Criado:** 2026-10-16
- **Gate 1:** **APROVADO** pelo usuário (2026-10-16)
- **Modelo IA:** GLM-5.3-Flash (z.ai) para todos os agentes
- **Repositório:** `/home/jovyan/testes/terraamazon-cleanroom-rust` → publicado em https://github.com/ils15/curupira-gis (commit `3b3eaa8`, tag `v0.1.0-alpha`, 2026-10-16)

## Decisões (Gate 1, 2026-10-16)

| Tema | Decisão |
|---|---|
| Modelo IA | GLM-5.3-Flash (z.ai) para todos os agentes |
| GPKG offline | **opt-in** via feature `offline-gpkg` (rusqlite bundled) |
| Migrate | dump SQL-text + ogr2ogr (`-t_srs EPSG:4674`); nunca roda PG10 |
| Bench Fase 3b | dataset sintético versionado |
| Repositório | `/home/jovyan/testes/terraamazon-cleanroom-rust` |
| GUI | egui + eframe sobre a trait `MapView` (única impl do MVP: egui; contrato migrável p/ MapLibre) |
| DB | sqlx + PostgreSQL 16 / PostGIS 3.4 + SRID 4674 (CHECK) |
| GDAL | 3.8, ligação **dinâmica**, feature opcional `gdal` |
| Licença | MIT OR Apache-2.0 (dual) + barreira clean-room |
| Roteamento runtime | general com lente de especialista |

## Fases

| Fase | Escopo | Estado |
|---|---|---|
| 0 | Scaffolding, licenciamento e barreira clean-room | **completa** (correção do registro libgdal abaixo) |
| 1 | Persistência PostGIS + migrações + `ta-cli migrate` | **completa** (2026-10-16) |
| 2 | IO geoespacial MVP + derivados (COG/MVT/FlatGeobuf) | **completa** (2026-10-16; ver seção Fase 2) |
| 3 | GUI egui+eframe sobre MapView + bench + security review | **completa** (2026-10-16; ver seção Fase 3) |
| 4 | Packaging + release (rebrand Curupira GIS, CI, release draft) | **pushed** (2026-10-16; commit `3b3eaa8` + tag `v0.1.0-alpha`; 1ª run de CI vermelha — causas raiz diagnosticadas e **correções locais aguardando commit** — ver seção Fase 4) |

## Verificação da Fase 0 (executada em 2026-10-16, host Linux)

| Check | Resultado |
|---|---|
| `cargo build --workspace` | ✅ verde (rustc/cargo 1.99.1 stable, instalado via rustup neste host) |
| `cargo fmt --all -- --check` | ✅ verde |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde |
| `cargo test --workspace` | ✅ 6 passed / 0 failed (SRID default 4674 + serde default, DbConfig, TARGET_SRID, contrato MapView) |

## Fase 2 — IO geoespacial MVP + derivados (completa, 2026-10-16)

### Entregue

| Item | Estado |
|---|---|
| `ta-gis::detect` | ✅ detecção por **extensão + magic bytes** (SHP `00 00 27 0A`, SQLite/GPKG, HDF5/NetCDF, DWG `AC10`, KML via sniff XML); **KML/NetCDF/DWG → `Error::UnsupportedFormat` tipado** com mensagem que sugere conversão (testes unitários) |
| `ta-gis::reader` (+ `vector_gdal`, `raster`) | ✅ `read_vector(path) -> VectorData {features (geo_types + attrs serde_json), srid, crs_wkt, layer_name}` via OGR (SHP/GeoJSON/GPKG/FGB); `read_raster_meta(path) -> RasterMeta {width, height, transform [6], bands, nodata, block_size, srid, crs_wkt, bbox}`; CRS extraído do dataset (auto-identify EPSG + WKT reportado); **ausente → `Error::MissingCrs`**; Z/M achatado p/ 2D; sem `unsafe` (API segura do crate `gdal` 0.19, link dinâmico 3.8.4) |
| CSV-XY fino (sem gdal) | ✅ `ta-gis::csv_xy` (feature `csv`): `--lat-col --lon-col --sep --encoding`; **sniff de delimitador** (`,` `;` `\t` fora de aspas, 5 primeiras linhas); **latin1→utf8** (byte→char 1:1); SRID default **4326** ou `--srid`; erros de parse com **linha/coluna** (`Error::CsvCell`); inferência leve de tipos nos attrs |
| `ta-gis::writer` (+ `ogr_writer`, `fgb_writer`) | ✅ `write_vector(features, path, format, opts)`: **SHP/GPKG via OGR** (`gdal`), **GeoJSON puro** (`geojson`), **FlatGeobuf puro** (`flatgeobuf` 6.0, geozero 0.15 — sem conflito de lock), **CSV-XY puro** (`csv`); schema de colunas inferido dos attrs (Int/Real/Text), sobrescrita limpa (remove destino + sidecars SHP); SRID de saída ≠4674 reprojeta (OGR); `export(repo, layer, format, out_path)` consumindo `FeatureRepo` (feature `db`) |
| `ta-gis::normalizer` | ✅ pipeline `prepare_import`: **detecta → lê → reprojeta p/ 4674 (OGR/PROJ)** → **valida** (coordenadas finitas, anéis fechados ≥4 pts, LineString ≥2) → **rejeita inválida com `Error::InvalidGeometry {index, reason}`** acionável (política documentada: ingestão NÃO repara silenciosamente — alinhado ao trigger `features_reject_invalid_geom`); `dominant_geom_type`/`bbox_of` p/ registro de camada |
| `ta-gis::derived` | ✅ **COG**: `translate_cog` = `gdal_translate -of COG` e `add_overviews` = `gdaladdo -r average` via `std::process::Command` (**args em array, sem shell**; caminhos com espaço seguros; ausente → `Error::ExternalTool` com instrução de instalar gdal-bin); **MVT** → redireção tipada p/ `ta_db::spatial::mvt_tile`; **FlatGeobuf** de arquivo → reader+writer |
| `ta-db::spatial::mvt_tile` | ✅ `ST_AsMVT` + `ST_TileEnvelope` (bounds geográficos 4674) + `ST_SimplifyPreserveTopology` com **tolerância proporcional ao zoom** (`360/2^z/extent`), `ST_AsMVTGeom` clip com buffer; **sqlx bind-only** (12 binds); `Ok(None)` p/ tile vazio; `mvt_available()` probe de protobuf-c |
| WMS cache (feature `wms`, default **off**) | ✅ `ta-gis::wms`: **reqwest async** (0.12 rustls — 0.13 trocou default TLS p/ aws-lc-rs/cmake), GetMap **1.3.0** com query canônica; cache em disco `~/.cache/ta/wms/<sha256>.png` **TTL configurável**; validação magic PNG + tamanho; gravação **atômica** (tmp+rename); `invalidate()` |
| CLI | ✅ `import-vector` (normalizer + bulk_insert + update_bbox; reporta **N features, bbox, srid→4674, tempo**; projeto por id/nome com criação automática; sufixo `_N` p/ camadas homônimas; migrações aplicadas idempotentemente), `import-raster` (meta apenas — **pixels nunca vão ao PG**; `source_uri` + bbox do transform reprojetado; `--cog` opcional gera derivado), `export` (5 formatos, `--srid-out`), `list-layers` (implementado de fato), `--help` completo |
| Fixtures sintéticas | ✅ `testdata/gen_fixtures.py` (seed **42**, determinístico, osgeo/`ogr2ogr`; **nada derivado do bundle legado**): SHP 200 polígonos irregulares EPSG:4674 + GeoJSON + GPKG equivalentes, CSV-XY 500 pontos (`;`), GeoTIFF 256×256 4 bandas UInt16 tiled 64 nodata=0; `manifest.json` com sha256; **todos < 1 MB**; gerados no working tree (commit aguarda aprovação) |
| Testes | ✅ **golden round-trip por formato** (import→WKB→export→re-import; casamento por `id`, coords com epsilon; SHP por tipo — formato não aceita misto), `UnsupportedFormat` (KML/NetCDF/HDF5/DWG), CSV sniff/latin1/linha-coluna, **reprojeção 4326→4674 com tolerância 1e-6°** + round-trip 4674→3857→4674 (1e-9°), MVT gated (`TA_TEST_PG=1`) decodificando protobuf Tile (tag `0x1A`, layer name, **version=2**) |

### Resultados (todos executados de verdade)

| Check | Resultado |
|---|---|
| `cargo build --workspace` | ✅ verde |
| `cargo fmt --all -- --check` | ✅ verde |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde |
| `cargo clippy ... --all-features` (inclui `wms`) | ✅ verde |
| `cargo test --workspace` | ✅ **85 passed / 0 failed** (unificação de features via ta-cli; PG tests pulam sem `TA_TEST_PG`) |
| `cargo test -p ta-gis --features gdal` | ✅ **42 passed** (fixtures reais: SHP/GeoJSON/GPKG 200 feats srid 4674; raster 256×256/4 bandas/block 64/nodata 0) |
| `cargo test -p ta-gis --features "gdal csv geojson flatgeobuf db wms"` | ✅ **56 passed** |
| `cargo check --workspace --all-features` | ✅ sem conflito de crates (flatgeobuf 6.0 usa geozero 0.15.1, mesma versão do workspace; reqwest 0.12 rustls/ring) |
| `scripts/test-pg.sh` (TA_TEST_PG=1) | ✅ suíte verde (pg_integration 5/5 + mvt_integration: 1 real-client-side + 3 **skips honestos** — ver desvio 1) |
| **E2E real via CLI** (harness PG) | ✅ import SHP 200 feats (srid 4674, bbox reportado, 180 ms) · import CSV 500 pts com **reprojeção 4326→4674** (200 ms) · import GeoJSON (auto-sufixo `_2`) · import-raster registra camada `Raster` com bbox do transform (**zero pixels no PG**) · `--cog` gera COG real (LZW tiled) e registra · export SHP/GPKG/FGB/GeoJSON/CSV verificados via `ogrinfo` (Feature Count 200/500, extent = bbox do import) · `list-layers` lista vetorial+raster |

### Notas / desvios da Fase 2

1. **MVT no harness conda: skip honesto** — o build conda-forge `postgis 3.4.3` sai **sem protobuf-c**: `ST_AsMVT` existe mas falha em runtime ("Compiled without protobuf-c support"). Os 3 testes MVT de rede fazem probe `mvt_available()` (`postgis_full_version() LIKE '%LIBPROTOBUF%'`) e pulam com mensagem; na imagem `postgis/postgis:16-3.4` (CI lane) o suporte existe e os testes rodam de verdade. O caminho SQL (bind-only) é exercitado nos testes client-side; o decodificador protobuf client-side, porém, só roda no teste **gated** (`TA_TEST_PG=1`) contra banco **com** protobuf-c — no harness conda atual nenhum byte protobuf é decodificado.
2. **Lote misto em FlatGeobuf via driver OGR** — o `FgbWriter` 6.0 trava o tipo da feature no primeiro elemento visto (`type_` sticky sob `Unknown` com `detect_type=false`); para lotes homogêneos a escrita é puro-Rust (feature `flatgeobuf`), para mistos o writer cai no driver OGR "FlatGeobuf" (feature `gdal`) — fallback sancionado pela tarefa; registrado em código e testes.
3. **SHP não aceita lote misto** — limitação do FORMATO (um shape type por arquivo), não do driver: `write_vector(Shapefile)` rejeita com erro acionável ("separe por tipo ou exporte GPKG/GeoJSON/FGB"); golden round-trip SHP roda um lote homogêneo por tipo.
4. **`shapefile`/`geozero` features removidas do ta-gis** — SHP é lido/escrito via GDAL (decisão da tarefa) e o geozero entra como dependência do caminho flatgeobuf; sem deps mortas.
5. **`NormalizationReport` simplificado** — `{input_srid, reprojected, n_features}`: os contadores `invalid/repaired_geometries` do stub não faziam sentido numa política fail-fast (rejeita na primeira inválida, sem reparo automático).
6. **ta-db estendido** (além do hook MVT): `FeatureRepo::list` (export), `LayerRepo::get` (export por id), `LayerRepo::set_bbox` (camadas raster), migração `0004_raster_layers.sql` (CHECK aceita `'raster'`), mapeamento `GeomType::Raster ↔ "raster"`.
7. **CLI aplica migrações idempotentes no `open_pool`** (primeiro uso em banco novo não exige passo manual).
8. **geo-types 0.7.20**: `Polygon::new` auto-fecha anéis e `Geometry` ganhou variantes `Line/Rect/Triangle` — validação cobre todas; reprojeção degrada `Rect/Triangle` para `Polygon` (documentado no código).
9. Fixtures **gerados mas não commitados** (instrução "não commit"); `manifest.json` permite verificar determinismo/regenerar; testes pulam com aviso se `testdata/` estiver vazio.
10. CI (GitHub Actions) ainda não tem service PG nem `libgdal-dev` — os builds default do workspace hoje puxam `gdal` via ta-cli (feature unification); CI precisará de `apt install libgdal-dev` + service postgis quando landar (R8 atualizado).
| `cargo deny check licenses advisories bans sources` | ✅ ok (cargo-deny 0.20.2 instalado e executado neste host) |
| `docker compose config` | ⚠️ docker ausente no host — YAML validado sintaticamente com PyYAML (imagem/portas/env/volume/healthcheck conferidos) |
| Features opcionais `ta-gis --features geojson,csv,geozero,shapefile` | ✅ `cargo check` verde |
| Feature opcional `ta-db --features offline-gpkg` (rusqlite bundled) | ✅ `cargo check` verde |
| Feature opcional `ta-gis --features gdal` | ✅ **verde** (2026-10-16, Fase 1) — o registro "libgdal-dev ausente" abaixo estava **incorreto**: `libgdal-dev 3.8.4+dfsg-3ubuntu3` está presente no host (descoberta do apollo no DISCOVERY.md; `libgdal.so.34` no ldconfig) |
| `git init` | ✅ feito (branch `main`); **nenhum commit** |
| CLI stubs | ✅ `import-vector`, `import-raster`, `list-layers`, `migrate` imprimem "not implemented yet" e saem com código **2** |

### Notas / desvios registrados

1. **egui/eframe 0.37**: a trait `eframe::App` mudou (`update` → `ui(&mut self, &mut Ui, &mut Frame)`) e `egui::Id::new` foi deprecada (usado `Id::unique`). Scaffold adaptado à API atual; `Area::show` ainda aceita `&Context`, então `RenderCtx` continua encapsulando o contexto egui.
2. **deny.toml**: política executada de verdade (cargo-deny 0.20.2):
   - licenças adicionadas à allow-list por aparecem no grafo real: `BSL-1.0`, `OFL-1.1` e `Ubuntu-font-1.0` (fonts embutidas do egui) — todas permissivas;
   - `RUSTSEC-2026-0192` (ttf-parser unmaintained, transitivo de egui/default_fonts) ignorado com justificativa — sem vuln ativa; revisar na atualização do egui;
   - `wildcards = "warn"` (não "deny"): `deny` conflita com herança de workspace (`ta-core.workspace = true` vira `*` interno);
   - copyleft (GPL/AGPL/MPL) **não** está na allow-list → `cargo deny check licenses` **falha** ao detectá-los (gate duro; o "warn GPL" do pedido foi implementado como falha de gate, mais restritivo).
3. **Cargo.lock**: commitado (workspace com binários) — gerado e presente.
4. Dependências pinadas nas versões estáveis atuais do crates.io (2026-10): eframe/egui 0.37, sqlx 0.9, clap 4.6, geo-types 0.7, geozero 0.15, gdal 0.19, rusqlite 0.39 (ajustado na Fase 1 — ver desvio 1 abaixo), thiserror 2, time 0.3.

## Verificação da Fase 1 (executada em 2026-10-16, host Linux)

### Entregas

| Item | Entregue |
|---|---|
| `ta-core::geo` (ponto único de conversão) | ✅ `to_wkb`/`from_wkb`/`to_ewkb`/`from_ewkb` reais via **geozero 0.15** (`with-wkb`+`with-geo`); EWKB com SRID embutido (flag `0x20000000`); SRID extraído do cabeçalho por helper testado; round-trip unitário para Point/LineString/Polygon/MultiPoint/MultiLineString/MultiPolygon, big-endian e buffers truncados |
| Migrations (sqlx migrate) | ✅ `crates/ta-db/migrations/0001_init.sql` (projects/layers, UNIQUE(project_id,name), CHECK geom_type/srid=4674, bbox geometry(Geometry,4674), FK CASCADE) e `0002_features.sql` (features BIGSERIAL, geom geometry(Geometry,4674) NOT NULL, attrs JSONB, **GIST**, CHECK `ST_SRID(geom)=4674` + **trigger `features_geom_validity`** que rejeita geometria inválida via `ST_IsValid` com erro claro — política documentada no comentário do arquivo) |
| `ta-db::repo` | ✅ traits atualizados ao contrato do Gate: `ProjectRepo::{create,get,list,delete}`, `LayerRepo::{add,list_for_project,update_bbox,delete}`, `FeatureRepo::{bulk_insert,bbox_query,count}`; impls `Pg*` sobre `&PgPool`/transação; **bind-only** (única interpolação: identificador sanitizado `ta_import_*` no CLI); bulk via **COPY FROM STDIN** (texto, EWKB hex) com **fallback batched INSERT** (`UNNEST($2::text[], $3::jsonb[])`); erros tipados `ta-db::Error` (thiserror, wrapping `sqlx::Error`/`MigrateError`) |
| `ta-db::spatial` | ✅ stub removido; `ensure_postgis` (erro legível com remedio quando extensão indisponível), `is_postgis_ready`, `run_migrations` (Migrator runtime — **sem macros compile-time**, risco R2; caminho resolvido de `CARGO_MANIFEST_DIR`) |
| `ta-cli migrate` | ✅ `--from-dir <dir> --to <DSN> [--dry-run]`; `.shp` → ogr2ogr (`-t_srs EPSG:4674 -lco GEOMETRY_NAME=geom -nln ta_import_<nome>`); `.sql` → raw multi-statement com aviso; `.tif` → listado e pulado (Fase 2); pós-import valida `ST_SRID(geom)=4674` tabela a tabela; destino recusado se PG < 16 ("nunca PG10"); senha mascarada em toda saída; erros legíveis; smoke test real executado (SHP 2 features + SQL aplicados, SRID 4674 ✓) |
| Testes gated | ✅ `crates/ta-db/tests/pg_integration.rs` — gated por `TA_TEST_PG=1` (sem a variável, imprimem "skipped" e passam, **compilam** no `cargo test` puro); migram banco NOVO criado/derrubado no teste (idempotência incluída) |

### Harness PostgreSQL (conda-forge, efêmero)

`scripts/test-pg.sh` — sem docker/sudo no host (DISCOVERY), o cluster roda do env conda:

1. cria `~/.conda/envs/ta-pg16` se não existir (`mamba create -c conda-forge postgresql=16 postgis=3.4`);
2. `initdb` + `pg_ctl` em dirs temporários (`/tmp/opencode/...`), porta **55432**, usuário `ta`, trust local;
3. `CREATE EXTENSION postgis`, exporta `TA_PG_DSN`, aplica migrações + roda a suíte gated e **derruba o cluster** ao sair (`--no-tests` mantém de pé).

**[UNKNOWN]→resolvido (R2):** o `mamba create` funciona; servidor **PostgreSQL 16.10 + PostGIS 3.4.3** (conda-forge) — PG16 major / PostGIS 3.4 minor, mesmo contrato do compose `postgis/postgis:16-3.4`. Desvio: pins EXATOS do DISCOVERY (16.15/3.4.3) **não resolvem** juntos hoje (conflito icu/libxml2 no solver); o script usa major/minor constraints e o solver fixou 16.10.

### Resultados (todos executados de verdade)

| Check | Resultado |
|---|---|
| `cargo build --workspace` | ✅ verde |
| `cargo fmt --all -- --check` | ✅ verde |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde (inclui `-p ta-db --features offline-gpkg`) |
| `cargo test --workspace` (sem banco) | ✅ 28 passed / 0 failed (unitários + integração gated compila e pula) |
| `scripts/test-pg.sh` → `TA_TEST_PG=1 cargo test --workspace` | ✅ integração 5/5: migrações em banco novo, round-trip 4674, rejeição SRID 4326, bulk 10k, bbox_query |
| `bulk_insert` 10k features | ✅ **356 ms** (orçamento documentado: 60s) via COPY FROM STDIN |
| EXPLAIN `bbox_query` | ✅ `Bitmap Index Scan on features_geom_gist` — GIST em uso |
| Trigger `ST_IsValid` | ✅ polígono "bow-tie" rejeitado com mensagem clara apontando ST_MakeValid na ingestão |
| `ta-cli migrate` smoke | ✅ dry-run + execução real: ogr2ogr reprojetou para 4674, SQL raw aplicado, relatório por tabela |
| `cargo check -p ta-gis --features gdal` | ✅ verde com libgdal-dev 3.8.4 (corrige registro da Fase 0; risco da Fase 2 morto cedo) |

### Notas / desvios da Fase 1

1. **rusqlite 0.40 → 0.39 (workspace)**: rusqlite 0.40 exige `libsqlite3-sys ^0.38`, incompatível com `sqlx-sqlite 0.9` (`<0.38`) no resolver — qualquer re-lock do workspace quebrava com `links = "sqlite3"`. 0.39 unifica em `libsqlite3-sys 0.37.x`. Impacto zero hoje: `offline-gpkg` é opt-in e não implementado. Revisar quando sqlx subir.
2. **sqlx sem macros `query!`** (risco R2): toda a API é runtime (`sqlx::query`/`query_as`/`query_scalar`/`raw_sql`/`Migrator::new`). `sqlx::raw_sql`/`query_scalar` em 0.9 exigem `SqlSafeStr` — SQL dinâmico (identificadores sanitizados) usa `AssertSqlSafe` com justificativa no local.
3. **`ta-cli migrate` não aplica as migrações do schema** — apenas garante PostGIS; tabelas `ta_import_*` são criadas pelo ogr2ogr. Migrações ficam no harness/testes.
4. CI ainda sem service PG (R8) — suíte gated mantém o pipeline verde; adicionar `postgis/postgis:16-3.4` como service num próximo passo.

## Estrutura entregue (Fase 0 + 1)

```
crates/
├── ta-core   # Project/Layer/Feature/Style, geo.rs (WKB/EWKB REAL via geozero), error.rs (thiserror)
├── ta-gis    # reader/writer/normalizer/derived stubs; features: gdal (✅ checka no host), shapefile, geojson, csv, geozero
├── ta-db     # pool.rs (PgPool), repo.rs (traits + impls Pg + COPY), spatial.rs (postgis/migrações), migrations/000{1,2}*.sql, tests/pg_integration.rs (gated)
├── ta-gui    # lib (map_view.rs: trait MapView + LayerHandle/Bbox/RenderCtx + MapViewEgui) + bin eframe janela vazia
└── ta-cli    # clap: import-vector, import-raster, list-layers (stubs, exit 2) + migrate (REAL: ogr2ogr + SQL-text + validação 4674)
raiz:
├── LICENSE-MIT / LICENSE-APACHE (copyright "terraamazon-cleanroom-rust contributors")
├── NOTICE.md (declaração clean-room) · CONTRIBUTING.md (barreira dura + sha256 legado)
├── deny.toml (validado) · docker-compose.yml (postgis 16-3.4) · .gitignore (deny-list binários legados)
├── .github/workflows/ci.yml · rust-toolchain.toml · README.md · .env.example
├── scripts/test-pg.sh (harness PG16+PostGIS conda, efêmero)
└── .pantheon/deepwork/terraamazon-cleanroom-rust/ (PLAN.md, STATUS.md, DISCOVERY.md, REVIEW.md)
```

## Pendências para fechar Fase 0

- [ ] Aprovação dos commits iniciais (fora do escopo desta subtask — nenhum commit feito).
- [x] `themis` review do gate F0 — **feito** no review final (ver REVIEW.md, veredito APPROVED_WITH_NOTES).
- [x] (Opcional) Verificar feature `gdal` em host com `libgdal-dev` — **feita na Fase 1**: `cargo check -p ta-gis --features gdal` ✅ verde (libgdal 3.8.4 presente no host).

---

## Verificação da Fase 3 (executada em 2026-10-16, host Linux)

### Entregas

| Item | Entregue |
|---|---|
| App shell (`crates/ta-gui/src/app.rs` + `panels.rs` + `attr_table.rs` + `dialogs.rs`) | ✅ dock esquerdo (árvore de camadas: visibilidade/ordem ▲▼/ativa/remover), canvas central (toolbar +/−/extent/novo ponto/undo/redo), painel direito (inspetor: CRS, estilo cor/espessura/opacidade, seletor de banda de raster), painel inferior (tabela atributiva com janela deslizante `show_rows`), status bar (cursor lat/lon EPSG:4674, escala 1:N, SRID, modo LOCAL/BANCO), menu (Arquivo/Ver/Camada/Ajuda) |
| Conexão | ✅ botão “Conectar banco” lê `TA_DATABASE_URL` do env (pré-preenchido) **ou** diálogo host/porta/banco/usuário/senha com percent-encoding dos componentes; usa `ta_db::pool::connect` + `ensure_postgis` + `run_migrations`; **sem banco = modo local** (camadas em memória, import direto) |
| MapViewEgui real (`map_view.rs` + `viewport.rs` + `draw.rs`) | ✅ pan (arraste), zoom (roda ancorada no cursor + botões ±, clamp de escala), render vetorial **rasterização direta no painter egui** (decisão documentada: tiny-skia descartado — painter já é tessellator 2D com clipping; fill só em anéis convexos, limitação pública do epaint), raster (GeoTIFF via ta-gis/gdal: downsample banda a banda UMA vez no carregamento → textura; “downsample p/ viewport” por frame = draw da textura escalada, O(viewport); seletor de banda), **LOD client-side documentado** (`viewport::lod_stride` p/ anéis: subpixel pulado + decimação por stride; `map_view::point_stride` p/ pontos: orçamento 20k), hit-test de pick (bbox pré-computado + ponto-em-polígono/distância com tolerância 6 px), repaint contínuo **somente durante interação** (`request_repaint` condicional a drag/zoom) |
| Edição básica | ✅ seleção por clique (pick → highlight amarelo + linha da tabela), tabela atributiva editável (commit em focus-loss; tipo inferido: número/bool/null/string) salvando via `FeatureRepo::update_attrs` (**adicionado ao repo, bind-only**), criar ponto (modo “novo ponto” na camada ativa; em banco insere via bulk e recarrega ids), deletar feature com modal de confirmação (`delete_feature` **adicionado, bind-only**), undo/redo **em memória** (stack de comandos de attrs; persiste no banco junto quando aplicável; histórico é limpo ao deletar — índices deslocariam) |
| Import via GUI | ✅ menu/diálogo (caminho + SRID opcional) → `ta-gis` (detect→read→normalize 4674) + `FeatureRepo::bulk_insert` em **thread de fundo** (`std::thread` + mpsc) com estágios na barra de progresso e cancelamento cooperativo entre etapas; UI nunca bloqueia (banco também em thread própria — `db_worker.rs`, runtime tokio single-thread dono do pool); erros em modal/banner legíveis |
| Bench de decisão | ✅ `scripts/gen_bench_fixtures.py` (seed 42, LCG, determinístico; manifest.json com sha256) + `crates/ta-gui/benches/map_render.rs` (headless, `run_pass`, 120 frames/cenário) + números reais em **`benches/map_render.md`** — ver abaixo. `testdata/bench/` gerado, **não commitado** (.gitignore exceto manifest) |
| A11y + i18n | ✅ atalhos: Ctrl+O abrir projeto, Ctrl+I importar, +/− zoom, Esc cancela modo de edição, Ctrl+Z/Y undo/redo (guard: não disparam com campo de texto focado); todos os widgets com rótulo textual (accesskit via egui/eframe nativo); temas dark/light do egui ambos usados (cores do tema, não hardcoded; paleta de camadas de tons médios nos dois); **i18n pt-BR/en** em `crates/ta-gui/src/i18n.rs` — enum `Key` + `tr()` exaustivos, 87 chaves, **100% dos rótulos da UI passam por ele**, teste de completude (não-vazia nos 2 idiomas + contagem) |
| Testes | ✅ contrato `MapView` com mock (`tests/map_view_contract.rs`) cobrindo os **5 métodos** + driver genérico; render headless sem GPU (`run_pass` + `tessellate`); pick acerta a feature certa pós-fit; lógica de viewport headless (round-trip tela↔mundo, zoom com âncora, pan, zoom-to-bbox, LOD stride monotônico, decimação, hit-test ponto/linha/polígono com buraco); i18n (chaves completas); app_state (undo/redo/append/extent); smoke headless da UI inteira (`tests/app_smoke.rs`: shell + 5 diálogos + import local end-to-end) — CI sem display/GPU |
| STATUS | ✅ esta seção |

### ta-db estendido (Fase 3 — edição pela GUI)

`FeatureRepo` ganhou 3 métodos **bind-only** (sem concatenação de SQL):
`list_with_ids(layer_id)` (id de linha + feature, mesma query ordenada —
sem zip de queries separadas), `update_attrs(feature_id, attrs)` (substitui
o JSONB) e `delete_feature(feature_id)`. Teste gated novo
(`edicao_por_id_update_e_delete`) em `tests/pg_integration.rs`.

### Bench — números reais (host, 2026-10-16; método completo em `benches/map_render.md`)

| cenário | mean (ms) | p95 (ms) | max (ms) |
|---|---|---|---|
| 10k_poligonos | 2.06 | 3.14 | 3.29 |
| 50k_pontos | 2.53 | 2.59 | 12.49 |
| ambos_mais_raster_1024 | 6.96 | 8.22 | 10.30 |
| raster_stress_8192 (só CPU headless) | 0.01 | 0.01 | 0.03 |

**Decisão 3b: gatilho NÃO disparado** — p95 pior caso 8.22 ms, 4× abaixo do
orçamento de 33 ms (30 fps). Sem fundamento para migrar a Tauri+MapLibre
nesta fase. Duas otimizações client-side documentadas fizeram a diferença:
LOD pontual levou 50k pontos de 158 ms p95 (sem LOD) para 2.6 ms; LOD de
anéis + culling por bbox mantêm 10k polígonos em ~2–3 ms. Limitação honesta:
o harness headless **não mede GPU** — o custo de preenchimento de raster
grande (gatilho 3b clássico) precisa do checklist de smoke manual em máquina
com display (listado em `benches/map_render.md`).

### Resultados (todos executados de verdade)

| Check | Resultado |
|---|---|
| `cargo build --workspace` | ✅ verde |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde |
| `cargo fmt --all -- --check` | ✅ verde |
| `cargo test --workspace` | ✅ **124 passed / 0 failed** (inclui 33 novos da Fase 3; suíte PG gated compila e pula sem banco) |
| `cargo bench -p ta-gui` | ✅ executado com dados reais gerados (seed 42); números acima |
| `scripts/gen_bench_fixtures.py --stress` | ✅ 10k SHP (1.1s) + 50k CSV (0.2s) + TIFF 1024² (1.7s) + TIFF 8192² (26s); manifest.json com sha256 |
| Import local end-to-end (headless) | ✅ teste: CSV 500 pontos → thread → normalização → camada no mapa |
| Bench DB (import 50k no PostGIS via GUI) | ⚠️ não executado nesta fase — PostGIS do harness conda não estava de pé; o caminho é o mesmo `bulk_insert` já validado na Fase 1 (356 ms/10k via COPY) e o worker usa o mesmo código. Re-executar no próximo ciclo com `scripts/test-pg.sh` |

### Notas / desvios da Fase 3

1. **API egui/eframe 0.37** (pin do workspace): `Panel::left/right/top/bottom` (ex-SidePanel/TopBottomPanel) com `show` (não `show_inside`, deprecada), `allocate_exact_size` devolve `(Rect, Response)`, `Color32` é premultiplicado (`to_srgba_unmultiplied` para hex), `Context::global_style()`, `run_pass`/`run_ui` headless, `Modal` nativo, `FullOutput::drop_without_applying_deltas()` obrigatório em testes headless (assert de Drop).
2. **Preenchimento de polígono só para anéis convexos** — limitação pública do epaint (`PathShape`: "fill is only supported for convex polygons"); anéis côncavos recebem contorno. Teste de convexidade por produto vetorial documentado em `draw.rs`.
3. **Diálogo de arquivo nativo não implementado** — import por caminho digitado (sem dep nova tipo rfd/gtk); documentado no hint do diálogo.
4. **"Abrir projeto" sem banco** mostra orientação (modo local não tem persistência de projeto); criar projeto no diálogo de projetos exige conexão.
5. **Undo/redo é de attrs** (escopo da fase); deletar feature limpa o histórico (índices deslocados invalidariam os comandos) — conservador e documentado.
6. **LOD pontual com orçamento 20k** — decisão de performance documentada em `map_view.rs`; zoom-in com <20k visíveis desenha todos.
7. **Smoke visual/GPU pendente de host com display** — checklist em `benches/map_render.md`; tudo que é lógica/UI-structure está coberto headless (124 testes).
8. **Sem `unsafe`; sem segredos** (senha nunca logada; DSN montado com percent-encoding); **zero `unwrap` em path de UI** (erros → banner/modal).

## Fase 4 — Packaging + release (EM PROGRESSO, iniciada 2026-10-16)

### Feito nesta fase (por @prometheus, aguardando commit/push de @iris)

| Item | Estado |
|---|---|
| Rebrand user-facing **Curupira GIS** | ✅ README.md reescrito (badges CI/release, screenshot placeholder, quickstart, nota clean-room/licenças); `NOTICE.md`/`CONTRIBUTING.md` mantidos |
| i18n | ✅ `AppTitle` → "Curupira GIS" (pt/en); `AppSubtitle` → one-liner; nova chave `AboutRepo` (sobre-dialog com hyperlink p/ github.com/ils15/curupira-gis) + teste de contagem atualizado 88→89 |
| Bin rename | ✅ `[[bin]] name = "curupira-gis"` em ta-gui (crate segue `ta-gui`); metadata `repository`/`homepage` no workspace + ta-gui; verificado: build/fmt/clippy/test verdes, bench e tests usam só a lib |
| CI `ci.yml` ampliado | ✅ 3 jobs: `linux` (fmt+clippy+test+deny), `linux-pg` (service `postgis/postgis:16-3.4`, `TA_TEST_PG=1`, `TA_PG_DSN` do service — MVT real aqui; protobuf-c na imagem), `windows` (vcpkg gdal clássico + cache binário, GDAL_LIB_DIR/GDAL_VERSION p/ gdal-sys 0.12, build+test). **Todas as actions pinadas por SHA** (checkout v6.1.0, setup-rust-toolchain v1.17.0, cache v6.1.0, upload-artifact v4.6.1, download-artifact v4.3.0 — SHAs verificados via API do GitHub em 2026-10-16) |
| Release `release.yml` | ✅ tag `v*` → matrix build (linux tar.gz / windows zip com DLLs + `share/proj`+`share/gdal`), checksums `SHA256SUMS`, SBOM CycloneDX (`cargo cyclonedx --format json --spec-version 1.5`), **release sempre DRAFT** (gate humano) |
| INSTALL.md | ✅ por OS (Windows: zip c/ GDAL embutida + PG16/PostGIS 3.4 via EDB/StackBuilder + `TA_DATABASE_URL`; Linux: apt libgdal + PG via compose; primeiro-run com fixtures `testdata/`) |
| `main.rs` | ✅ helper `use_bundled_gdal_data()` aponta `PROJ_LIB`/`GDAL_DATA` p/ dados ao lado do exe (unsafe de `set_var` edition-2024 justificado: roda single-thread no início de `main`) |

### Decisões da Fase 4

1. **cargo-dist descartado (por agora) → workflow manual de release.** Motivos: (a) o workflow do cargo-dist é gerado por `dist init`, impossível de produzir/validar neste host; (b) 2 alvos não justificam a maquinaria de 20; (c) workflow manual 100% auditável e SHA-pinado. Documentado no README (§ Releases); reavaliar pós-alpha. `x86_64-unknown-linux-gnu` (tar.gz) + `x86_64-pc-windows-msvc` (zip; **MSI pós-alpha**).
2. **Windows/GDAL via vcpkg clássico, features ON** — contrato verificado no fonte do `gdal-sys` 0.12.0: procura `gdal_i.lib` em `GDAL_LIB_DIR`/`GDAL_HOME\lib`; `GDAL_VERSION` env é obrigatório no Windows (sem pkg-config) e os bindings pré-gerados cobrem GDAL 3.4–3.12. O job extrai a versão real do `vcpkg list gdal`.
3. **Clippy no Windows sem `-D warnings`** (erros travam; warnings não) — primeiro contato com o alvo; endurecer após a primeira run verde.

### Verificação executada (host Linux, 2026-10-16) — HONESTO

| Check | Resultado |
|---|---|
| `cargo build --workspace` | ✅ verde; `target/debug/curupira-gis` + `ta-cli` produzidos |
| `cargo fmt --all -- --check` | ✅ verde |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde |
| `cargo test --workspace` | ✅ 125 passed / 0 failed (gated PG imprimem "skipped") |
| YAMLs (ci.yml, release.yml) | ✅ parse PyYAML + assertions: 3 e 4 jobs, todas as `uses:` com SHA de 40 hex |
| `actionlint` | ⚠️ **ausente no host** — validação limitada a sintaxe YAML/semântica GHA manual; rodar actionlint no primeiro push |
| **Build Windows** | ❌ **NÃO PROVÁVEL neste host (Linux)** — o job `windows` é construído sobre o contrato verificado do gdal-sys; a **primeira run de CI no push é a prova real** |

### Push executado (@iris, 2026-10-16 — aprovado explicitamente pelo usuário)

| Item | Resultado |
|---|---|
| Auth | ✅ token `~/.git-credentials` (helper `store`), scopes `repo, workflow`; `gh` CLI ausente — API REST usada |
| Commit | ✅ `3b3eaa8` (root-commit, branch `main`): 93 arquivos, 22.196 linhas, **1,65 MB** (sem `target/` 18G, sem `testdata/bench/*` grandes, sem legado — `.gitignore` barrou) |
| Push | ✅ `main` → `origin` (`https://github.com/ils15/curupira-gis`), verificado via `git ls-remote` |
| Tag | ✅ `v0.1.0-alpha` (anotada) pushed — dispara `release.yml` (trigger `v*`) → **release DRAFT** |
| Run CI | ✅ disparada (push `main`), `in_progress`: jobs `linux`, `linux-pg` (PostGIS 3.4/MVT), `windows` (vcpkg/gdal-sys) → https://github.com/ils15/curupira-gis/actions/runs/38944639843 |
| Run release | ✅ disparada (push tag), `in_progress` → https://github.com/ils15/curupira-gis/actions/runs/38944663060 |
| Draft release | ⏳ NÃO publicado — gate humano (revisar artefatos/checksums/SBOM antes de publish) |

### 1ª run de CI — falhas, diagnóstico e correções (@prometheus, 2026-10-16)

Resultado da 1ª run (push `3b3eaa8` + tag `v0.1.0-alpha`): `linux-pg` ❌ (~2min, exit 101 no
step de testes), `windows msvc` ❌ (~1,5min, step `vcpkg install`), `release windows` ❌ (~1min,
mesmo step). `linux` e `release linux` seguiam em progresso (ignorados). Logs de job não
baixáveis sem admin (API 403) — diagnóstico por: **(a)** steps/conclusões via API pública de
jobs, **(b)** reprodução local integral da suíte gated com o harness PG (conda) e **(c)** prova
numérica dos envelopes `ST_TileEnvelope` no PostGIS 16/3.4 local.

**linux-pg — causa raiz: bugs de coordenada de tile nos testes MVT** (`mvt_integration.rs`).
Os 3 testes que exigem `ST_AsMVT` real **nunca haviam rodado em host algum** (o harness conda
não tem protobuf-c → `[skip]`; a imagem docker do CI tem → rodaram pela 1ª vez e falharam):

| Teste | Coordenada antiga | Falha provada (PostGIS local) | Correção |
|---|---|---|---|
| `mvt_tile_tem_estrutura_valida_e_versao_2` | `(6,25,38)` | envelope = lon −39.4..−33.8 (Atlântico) → tile vazio → `Ok(None)` → panic no `.expect` | `(3,2,4)` — lon −90..−45, lat −22.5..0, **contém o lote sem clipping** |
| `mvt_zoom_alto_simplifica_mais_que_zoom_baixo` | `(5,25,38)` | `ST_TileEnvelope: Invalid tile y value, 38` (y máx = 2⁵−1 = 31) → erro de DB → panic | z5 `(5,11,18)` (contém o polígono inteiro) + z16 `(23302,37150)` (tile SOB o polígono, interior do anel) |
| `mvt_area_vazia_e_none` | `(6,62,25)` | ✅ correta (Pacífico vazio) — passou no CI | inalterado |

Envelopes novos **provados via SQL** no PostGIS 16/3.4 local (`ST_TileEnvelope` com bounds
geográficos divide LINEARMENTE: lat = 90 − y·180/2^z, sem Mercator no eixo y). Helpers `z_x`/`z_y`
removidos (ficariam `dead_code` → clippy `-D warnings` quebraria o job linux). DSN/env
`TA_PG_DSN`/`TA_TEST_PG` conferidos: batem com os testes — NÃO era a causa.

**windows (ci) e release windows — causa raiz: diretório do binary cache do vcpkg inexistente.**
`VCPKG_DEFAULT_BINARY_CACHE` aponta para `vcpkg-bincache` sob o workspace, **nunca criado**:
na 1ª run não há cache a restaurar e o `actions/cache` não cria o path; o vcpkg exige o
diretório já existente (grava dentro, não cria) → `vcpkg install gdal:x64-windows` morria no
startup em 14–21s, antes de compilar qualquer pacote. Correção (idempotente, nos 2 workflows):
`New-Item -ItemType Directory -Force` antes do install, com `shell: pwsh` explícito. Endurecido
também o regex de extração de `GDAL_VERSION` (formatos antigo/novo de `vcpkg list`; grupo 2;
erro imprime o listing — seria a próxima falha silenciosa). `GDAL_LIB_DIR` + `GDAL_VERSION`
mantidos (contrato do `gdal-sys` 0.12 verificado na decisão 2 da Fase 4).

### Pendências pós-push (Fase 4)

1. Acompanhar run de CI — **principalmente job `windows`** (vcpkg/gdal-sys: primeira prova real; `actionlint` não existia no host, validação foi manual).
2. Conferir job `linux-pg` (testes MVT/integração contra PostGIS 3.4).
3. Revisar o **draft release** gerado pela run `release` (artefatos linux/windows, `SHA256SUMS`, SBOM CycloneDX) e publicar manualmente após aprovação.
4. Este STATUS.md foi atualizado **localmente após o push** — a versão publicada no repo está desatualizada; publicá-la exige um segundo commit (aguardando instrução).
5. **Commitar e pushar as correções da 1ª run** (3 arquivos: `ci.yml`, `release.yml`, `mvt_integration.rs` + este STATUS.md) — @iris. A 2ª run de CI é a prova das correções; se o job windows avançar do `vcpkg install`, monitorar o **link** do gdal-sys e a extração de `GDAL_VERSION` (regex endurecida p/ ambos os formatos de `vcpkg list`).

