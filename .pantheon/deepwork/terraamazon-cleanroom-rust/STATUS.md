# STATUS — terraamazon-cleanroom-rust

- **Slug:** `terraamazon-cleanroom-rust`
- **Criado:** 2026-10-16
- **Gate 1:** **APROVADO** pelo usuário (2026-10-16)
- **Modelo IA:** GLM-5.3-Flash (z.ai) para todos os agentes
- **Repositório:** `/home/jovyan/testes/terraamazon-cleanroom-rust` → publicado em https://github.com/ils15/curupira-gis (commit `3b3eaa8`, tag `v0.1.0-alpha`, 2026-10-16)
- **Re-planejamento (2026-10-18):** aprovado o plano de **paridade do fluxo de monitoramento** (fases `0`–`6`) → ver `.pantheon/deepwork/terraamazon-cleanroom-rust/PLAN-terraamazon-parity.md` e a seção final deste arquivo. **A Fase 0 desse novo plano está concluída; a Fase 1 está implementada e revisada (aguardando commit); as Fases 2–6 estão pendentes.** As seções históricas abaixo (v0.1–v0.2.2) permanecem válidas e **não** foram alteradas.

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

### Entregáveis pedidos pelo usuário: instalador .exe / .deb / macOS (EM PROGRESSO, @prometheus, 2026-10-17)

Complemento do draft: 3 artefatos NOVOS além do zip Windows + tar.gz Linux +
SBOMs existentes (que ficam intocados). Jobs existentes do `release.yml`
preservados — apenas steps adicionados ao final + novo job `macos`.

| Item | Estado |
|---|---|
| **Windows: instalador `CurupiraGIS-Setup-x64.exe`** (Inno Setup) | 🔄 `packaging/windows/curupira.iss` criado (AppVersion via /DVERSION numérico, id fixo, licença MIT, pt-BR+en, atalhos menu iniciar/desktop, DLLs+`share\gdal`+`share\proj` alinhados ao layout `<exe>\share\...` do helper `use_bundled_gdal_data()` do main.rs — **nenhuma** env gravada no registro). Steps novos no job windows: localizar `ISCC.exe` (PATH → caminhos padrão → **fallback `choco install innosetup -y --no-progress`**) + staging `dist\installer-stage` (mesma composição do zip) + artifact `dist-windows-setup` |
| **Linux: `curupira-gis_x64.deb`** (cargo-deb) | 🔄 `[package.metadata.deb]` em crates/ta-gui (pacote `curupira-gis` com os DOIS bins em `/usr/bin` + docs; `depends = "$auto"`); steps novos no job linux (cache + `cargo install cargo-deb --locked` — **não apt**, versão 1.42 do ubuntu-24.04 diverge da 3.x validada) + artifact `dist-linux-deb`; tar.gz existente mantido |
| **macOS: `CurupiraGIS-macos-arm64.zip`** (novo job) | 🔄 job `build-macos` (macos-latest/arm64): `brew install gdal pkg-config` + build + `packaging/macos/bundle.sh` (monta `CurupiraGIS.app`, walk recursivo `otool -L` copiando dylibs não-sistema p/ `Contents/Frameworks`, reescrita `install_name_tool -change`/`-id` p/ `@rpath` + `-add_rpath`, **re-sign ad-hoc obrigatória pós-modificação no arm64**, zip via ditto) + artifact `dist-macos`; README/INSTALL: nota Gatekeeper (botão direito → Abrir) |
| Release job | 🔄 `needs` + `build-macos`; checksums agora incluem `*.exe *.deb`; notas do draft citam os 3 novos artefatos + Gatekeeper |

### Decisões destes entregáveis (2026-10-17)

1. **macOS GDAL: brew corrente + bindings pré-gerados do gdal-sys 0.12** (contrato 3.4–3.12, ver decisão 2 abaixo). NÃO há `gdal@3.12` versionado no homebrew-core; feature `bindgen` (plano B documentado no YAML com `::warning` se brew > 3.12) exigiria LIBCLANG_PATH do llvm do brew — mais lento/fragil. `pkg-config` resolve versão/flags no macOS (sem GDAL_VERSION env).
2. **depends=$auto no .deb** em vez de fixar `libgdal35 | libgdal34`: dpkg-shlibdeps resolve a libgdal REAL do Ubuntu que gerou o pacote.
3. **Instalador espelha o layout do zip** (`{app}\*.exe|*.dll` + `{app}\share\...`): zero mudança no main.rs; PROJ_LIB/GDAL_DATA seguem sendo resolvidos em runtime pelo helper existente.
4. **cargo-deb via cargo install (3.x) com cache**, não apt (1.42 de 2021 divergente do que foi validado localmente).

### Verificação executada (host Linux, 2026-10-17) — HONESTO

| Check | Resultado |
|---|---|
| `release.yml` | ✅ PyYAML parse + assertions: 5 jobs (`build-linux`, `build-windows`, `build-macos`, `sbom`, `release`), todas as `uses:` com SHA de 40 hex, steps/artifacts novos presentes (`dist-linux-deb`, `dist-windows-setup`, `dist-macos`), fallback choco presente |
| `cargo build --workspace` / fmt / clippy `-D warnings` | ✅ verdes (após incluir `[package.metadata.deb]`) |
| **`.deb` metadata** | ✅ **validado de verdade**: `cargo install cargo-deb` (3.8.0, <1 min) → `cargo deb -p ta-gui --no-build` gerou `curupira-gis_0.1.0-1_amd64.deb` (8,1 MB) com os 2 bins em `/usr/bin`, docs, copyright e **`Depends: libc6 (>= 2.39), libgdal34t64 (>= 3.0.0)`** — $auto resolveu a libgdal do sistema (host 24.04: `libgdal34t64`; runner resolverá libgdal34/35 equivalente). Nota: exigiu `LD_LIBRARY_PATH` limpo — o conda deste host contamina a resolução do dpkg-shlibdeps (não existe no runner) |
| `bundle.sh` (lógica, sem macOS no host) | ✅ simulado com stubs de `otool`/`install_name_tool`/`codesign`/`ditto`/`cp` (`/tmp/opencode/bundle-mock`): walk transitivo correto (libproj copiada via dependência da cópia do gdal), sistema (/usr/lib, /System) e `@*` ignorados, `-id`/`-change`/`-add_rpath` com args exatos, codesign nas 2 dylibs + 2 exes, **idempotente** (run 2 == run 1, byte a byte no log), Info.plist XML válido |
| `bash -n bundle.sh` | ✅ |
| `curupira.iss` | ⚠️ **revisão manual linha a linha** (Inno Setup não compila em Linux): paths relativos ao .iss conferidos contra o staging montado pelo job (`dist\installer-stage`), sintaxe ISPP ({#...}/#ifndef/#error) e constantes Inno 6 conferidas em documentação de memória — **[UNKNOWN] compilação real do ISCC** |
| ISCC.exe pré-instalado no windows-latest | ⚠️ não provável neste host — step tem 3 níveis (PATH → `Program Files (x86)/Inno Setup 6` → choco) e falha com mensagem clara se todos faltarem |
| brew GDAL > 3.12 no runner | ⚠️ [UNKNOWN] versão atual do brew no macos-latest (websearch indisponível no host em 2026-10-17) — se > 3.12, gdal-sys falha cedo com erro explícito; plano B documentado no YAML |

### Risco residual — "a run #1 do novo release prova:" (resolução na próxima tag)

1. **ISCC compila o .iss** (língua pt-BR: nome real do .isl no Inno do runner — o workflow resolve e injeta via `/DBR_ISL`; fallback estático do .iss cobre compilação manual). Se o Inno do runner for < 6.3, `ArchitecturesInstallIn64BitMode=x64compatible` falha → trocar p/ `x64` ( improvável: imagem 2026).
2. **dpkg-shlibdeps no runner limpo** resolve `libgdal34/35` p/ o Depends (aqui provado com libgdal34t64; mesma mecânica).
3. **brew GDAL ≤ 3.12** no macos-latest (senão: plano B bindgen, documentado no job).
4. **codesign ad-hoc do bundle** roda no arm64 (re-sign pós `install_name_tool` está no script; o stub validou só a sequência, não o codesign real).
5. **`ditto`/`otool`/`install_name_tool` disponíveis** no runner (nativos do macOS — risco baixo).

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

### Run #2 (2026-10-16, commit 79af637) — workflow `ci`

| Job | Resultado | Detalhe |
|-----|-----------|---------|
| `linux` (fmt + clippy + test, sem PG) | ✅ passou | — |
| `linux-pg` (pg16/postgis 3.4, testes gated) | ❌ **falhou** (~1 min) | Ver root cause abaixo — **correção aplicada localmente** |
| `windows` (msvc + GDAL vcpkg) | ⏳ pendente | Ainda em execução no momento do diagnóstico |

#### Root cause do `linux-pg` na run #2

**Não** era service/healthcheck (container `postgis/postgis:16-3.4` ficou healthy
em ~6 s), **nem** DSN/env (`TA_PG_DSN` correto, 3 testes gated passaram), **nem**
falta de setup: a extensão PostGIS e as migrações subiram via `init_schema()`.

Causa real: `mvt_tile_tem_estrutura_valida_e_versao_2` — decoder protobuf
escrito à mão em `crates/ta-db/tests/mvt_integration.rs` (`decode_layer`) fazia
`break` no primeiro tag desconhecido. O protobuf-c do PostGIS serializa a
mensagem `Layer` com `name` (campo 1) primeiro e `version` (campo 15) por
último, após features/keys/values/extent → `version` nunca era alcançado
(`left: None, right: Some(2)` em `mvt_integration.rs:245`).

Esse caminho nunca havia rodado antes: o harness conda-forge compila PostGIS
**sem** protobuf-c (`postgis_full_version()` sem `LIBPROTOBUF`), então os
testes de MVT pulavam localmente — o CI (imagem com protobuf-c) é a primeira
execução real, e ela reprovou.

#### Correção

- `decode_layer` reescrito como walker protobuf correto: lê tags como varint,
  e **pula** campos desconhecidos pelo wire type (0/1/2/5) em qualquer ordem.
- Teste de regressão ungated novo:
  `decode_layer_pula_campos_e_acha_versao_apos_desconhecidos` (bytes
  sintéticos com ordem adversa: name → extent → features → version).

#### Verificação local (simulando o fluxo do CI)

- Cluster conda pg16/postgis 3.4 (`scripts/test-pg.sh --no-tests`), DSN no
  mesmo formato do workflow (`postgres://ta:ta@127.0.0.1:<porta>/ta`).
- `TA_TEST_PG=1 cargo test --workspace` completo: **todos os binários verdes**
  (mvt_integration 5 ✅ incl. regressão — MVT real continua skip no harness
  conda por falta de protobuf-c; pg_integration 6 ✅ contra PG real).
- `cargo fmt --check` ✅ · `cargo clippy --workspace --all-targets -D warnings` ✅.

#### Risco para a run #3

`cargo test` aborta no primeiro binário reprovado, então `pg_integration`
real (via service) **nunca chegou a rodar** nas runs #1–2. Com o decoder
corrigido ele executará no CI pela primeira vez — verde localmente contra
PG real (harness), mas é a parte não comprovada em CI até a próxima run.

### Run #3 — CI no push (2026-10-16)

| Job | Resultado |
|-----|-----------|
| `linux` (fmt + clippy + test, sem PG) | ✅ verde |
| `linux-pg` (Postgres 16 / PostGIS 3.4, `TA_TEST_PG=1`) | ✅ verde — **MVT real passou**: tile decodificado do `ST_AsMVT` do PG |
| `windows` (msvc, GDAL via vcpkg) | ❌ falhou no step "Exporta GDAL_LIB_DIR e GDAL_VERSION p/ gdal-sys" |

#### Falha do windows (run #3)

`gdal_i.lib não encontrado em C:\vcpkg\installed\x64-windows\lib` — o vcpkg
instalou GDAL 3.12.4 com sucesso (~1.5h de build, **binary cache gravado**;
próxima run restaura e é rápida), mas gera o import library como `gdal.lib`,
enquanto o `build.rs` do gdal-sys 0.12 no MSVC procura `gdal_i.lib` (naming do
SDK oficial antigo). Sem `gdal_i.lib` em `GDAL_LIB_DIR` ou `GDAL_HOME\lib`, o
build script morre com "windows-msvc requires gdal_i.lib".

#### Correção (aplicada em ci.yml + release.yml, mesmo step)

1. **Normalização do import library**: se `gdal_i.lib` não existe, o step
   procura `gdal*.lib` (preferindo o `gdal.lib` canônico do vcpkg) e faz
   `Copy-Item gdal.lib gdal_i.lib` no mesmo dir. Se nada existe, o erro lista
   os `*.lib` presentes (diagnóstico futuro). Idempotente: se `gdal_i.lib`
   já existe, não faz nada.
2. **`GDAL_VERSION` endurecida**: regex principal + fallback (1ª linha
   iniciada por `gdal` → primeiro `X.Y.Z`) p/ tolerar drift de formato do
   `vcpkg list`. Bindings pré-gerados do gdal-sys 0.12 cobrem GDAL 3.4–3.12
   (`3_12` confirmado no crate) — 3.12.4 usa os bindings `3_12`.
3. **DLLs em runtime**: `installed\x64-windows\bin` no `GITHUB_PATH`. Já
   existia no ci.yml; adicionado ao release.yml por simetria. Sem isso o
   build liga (import library) mas o primeiro binário executado morre com
   `0xc0000135` (`STATUS_DLL_NOT_FOUND`).
4. Linux jobs intocados.

#### O que a run #4 vai provar (verificação que não dá p/ fazer no host Linux)

1. `vcpkg install` restaura do binary cache (minutos, não 1.5h) e o step
   "Exporta" loga `Import library normalizada: gdal.lib -> gdal_i.lib`.
2. `GDAL_VERSION=3.12.4` extraído e exportado (bindings `3_12` selecionados).
3. `cargo clippy`/`build`/`test` do windows verdes — valida (a) o link MSVC
   contra a import library renomeada (`static=gdal_i` → import de `gdal.dll`)
   e (b) o load da `gdal.dll` + dependências transitivas via PATH no primeiro
   binário executado (`cargo test`).

### Run #3 — release (tag, 2026-10-17): 2 jobs falharam (macOS + Windows)

| Job | Resultado |
|-----|-----------|
| `build-linux` | ✅ verde (tar.gz + .deb) |
| `sbom` | ✅ verde |
| `build-windows` | ❌ falhou no step "Compila instalador (Inno Setup)" |
| `build-macos` | ❌ falhou no build.rs do gdal-sys |
| `release` (draft) | ⏭️ pulado (`needs` reprovado) |

#### Causa 1 — macOS: GDAL 3.13 sem binding pré-gerado

`gdal-sys 0.12 build.rs:282 — panic: No pre-built bindings available for GDAL
version 3.13`. O Homebrew subiu p/ GDAL 3.13 e os bindings pré-gerados do
gdal-sys 0.12 vão até 3.12 (`prebuilt-bindings/3_4..3_12`) — exatamente o
`::warning::` previsto no step antigo. Sem feature bindgen, o build.rs só copia
o binding pré-gerado da versão e morre.

**Fix (com contratos verificados no fonte do registry):**

1. `crates/ta-gis/Cargo.toml`: feature nova `bindgen-gdal = ["gdal/bindgen"]`.
   O gdal 0.19.0 reexporta: `bindgen = ["gdal-sys/bindgen"]` (seção [features]
   do Cargo.toml do crate); habilitar `dep/feat` ativa o dep opcional
   implicitamente.
2. `release.yml` (job macOS): build virou
   `cargo build --release -p ta-gui -p ta-cli --features ta-gis/bindgen-gdal`
   — ambos os pacotes dependem de ta-gis (ta-gui direto + feature `gdal`;
   ta-cli via default feature) e geram os 2 binários da release. Com bindgen
   não há teto de versão: bindings gerados dos headers via libclang, mesma
   allowlist do prebuilt; `GDAL_VERSION` env não é usado nesse caminho (só
   alimentaria o prebuilt) — macOS nunca o definiu.
3. Fallback de libclang (`brew install llvm` + `LIBCLANG_PATH`) incluído
   **comentado** no workflow com instrução de quando ativar (runners macOS têm
   Xcode CLT; clang-sys normalmente resolve).

#### Causa 2 — Windows: shim do Chocolatey sem diretório `Languages`

`Get-Command ISCC.exe` achou o **shim** (`C:\ProgramData\Chocolatey\bin\
ISCC.exe`), e `Split-Path` do shim derivou `$langsDir =
C:\ProgramData\Chocolatey\bin\Languages` → `Get-ChildItem` explodiu ("Cannot
find path"; o shell pwsh do GitHub promove erro não-terminal a falha).

**Fix (step "Localiza ISCC.exe real"):** resolução do ISCC.exe REAL em ordem —
(1) `%ProgramFiles(x86)%\Inno Setup 6`, (2) `%ProgramFiles%\Inno Setup 6`,
(3) registry (uninstall keys, `DisplayName -like 'Inno Setup*'` →
`InstallLocation`), (4) tools do choco
(`C:\ProgramData\chocolatey\lib\innosetup\tools\**\ISCC.exe` recursivo),
(5) último recurso `choco install innosetup` + re-derivar do (4).
`$langsDir` agora deriva do dir do ISCC real. Degradação graciosa: sem
`*.isl` brazil em `Languages\`, compila **SEM /DBR_ISL** (EN-only) +
`::warning::pt-BR ISL não encontrado` — idioma nunca mais falha o job.
Para o caminho EN-only funcionar, `curupira.iss` mudou: a entrada pt-BR em
[Languages] ficou sob `#ifdef BR_ISL` (o fallback estático antigo
`#ifndef BR_ISL → BrazilianPortuguese.isl` exigiria o arquivo mesmo sem a
flag e quebraria o modo EN-only).

#### Verificação local (host Linux, 2026-10-17)

| Check | Resultado |
|-------|-----------|
| pyyaml release.yml + ci.yml | ✅ (jobs: 5 e 3, estrutura íntegra) |
| `cargo build --workspace --offline` | ✅ 2,6s (cache quente; lock intacto no grafo default) |
| `cargo build -p ta-gui -p ta-cli --features ta-gis/bindgen-gdal` | ✅ 8,0s (conjunto de args exato do workflow, perfil dev) |
| Prova do caminho bindgen | ✅ `output` do build-script com **0** `rustc-cfg` (o prebuilt emitiria `gdal_sys_3_8_4`); `bindings.rs` gerado na hora por rust-bindgen **0.72.1** via libclang-18 contra GDAL **3.8.4** do host (403 KB vs 346 KB do prebuilt 3_8) |
| `cargo metadata --features ta-gis/bindgen-gdal` | ✅ `bindgen-gdal` na feature list do ta-gis |
| `cargo tree -e features -i gdal-sys` (com a feature) | ✅ cadeia `ta-gis/bindgen-gdal → gdal/bindgen → gdal-sys/bindgen`; sem a feature: só `default` (controle) |
| Sintaxe PowerShell dos steps Windows | ⚠️ revisão manual (pwsh não existe no host; pyyaml valida o YAML) |

`Cargo.lock` ganhou as entradas opcionais do bindgen (bindgen 0.72.1,
clang-sys 1.9.1, cexpr…) — comportamento esperado: a lockfile registra deps
alcançáveis por features do workspace p/ resolução offline posterior. Sem
`--locked` no CI, inocuo; commitar junto.

#### Não comprovável no host ([UNKNOWN] p/ a run #4)

1. macOS: libclang do Xcode CLT no runner real (clang-sys costuma achar; se
   falhar, descomentar o step de fallback `brew llvm` já presente no yml).
2. Windows: execução real dos steps PowerShell (registry/shim do runner);
   degradação EN-only revisada por leitura, não executada.
3. Risco latente (documentado): se o vcpkg subir GDAL > 3.12, o job Windows
   precisará do mesmo `bindgen-gdal` — hoje (3.12.4) continua no prebuilt
   `3_12` via `GDAL_VERSION` env, intocado.

#### Arquivos p/ @iris (commit + push, sem publicar release)

1. `crates/ta-gis/Cargo.toml` — feature `bindgen-gdal`
2. `Cargo.lock` — entradas opcionais do bindgen (consequência da 1)
3. `.github/workflows/release.yml` — build macOS com feature + fallback
   libclang comentado + resolução do ISCC real + degradação EN-only
4. `packaging/windows/curupira.iss` — pt-BR sob `#ifdef BR_ISL`
5. Este STATUS.md

### Pendências pós-push (Fase 4)

1. Acompanhar run de CI — **principalmente job `windows`** (vcpkg/gdal-sys: primeira prova real; `actionlint` não existia no host, validação foi manual).
2. Conferir job `linux-pg` (testes MVT/integração contra PostGIS 3.4).
3. Revisar o **draft release** gerado pela run `release` (artefatos linux/windows, `SHA256SUMS`, SBOM CycloneDX) e publicar manualmente após aprovação.
4. Este STATUS.md foi atualizado **localmente após o push** — a versão publicada no repo está desatualizada; publicá-la exige um segundo commit (aguardando instrução).
5. **Commitar e pushar as correções da 1ª run** (3 arquivos: `ci.yml`, `release.yml`, `mvt_integration.rs` + este STATUS.md) — @iris. A 2ª run de CI é a prova das correções; se o job windows avançar do `vcpkg install`, monitorar o **link** do gdal-sys e a extração de `GDAL_VERSION` (regex endurecida p/ ambos os formatos de `vcpkg list`).

### Run #4 — release (tag, 2026-10-17): macOS + Windows falharam (fix local, aguardando commit)

Run [39019341582](https://api.github.com/repos/ils15/curupira-gis/actions/runs/39019341582):

| Job | Resultado |
|-----|-----------|
| `build-linux` | ✅ verde |
| `sbom` | ✅ verde |
| `build-windows` | ❌ exit 1 no step "Compila instalador (Inno Setup)" — **build do cargo passou** (GDAL 3.12.4 via vcpkg, `GDAL_VERSION: 3.12.4` no log) |
| `build-macos` | ❌ exit 101 compilando o crate `gdal` 0.19 (wrapper safe) |
| `release` (draft) | ⏭️ pulado (`needs` reprovado) |

#### Causa 1 — macOS: bindgen gerou os bindings, mas o WRAPPER do gdal 0.19 não suporta GDAL 3.13

Log do job (linhas citadas do `jobs/117357839527/logs`):

- `error[E0425]: cannot find value GDT_Byte in module GDALDataType` →
  `gdal-0.19.0/src/raster/types.rs:35` e `:342`;
- `error[E0063]: missing field bOperateInBufType in initializer of GDALRasterIOExtraArg` →
  `gdal-0.19.0/src/raster/rasterband.rs:282`;
- `error: could not compile gdal (lib) due to 3 previous errors` → exit 101.

Ou seja: o bindgen cumpriu o papel dele (bindings geraram da API C 3.13), mas
o **wrapper safe** do crate (tipos/enum de raster) ainda espera a superfície
3.12 — o problema não é só "teto dos bindings", é **compat do wrapper**. A
alternativa bindgen (run #3→#4) está descartada enquanto o gdal-rs não
declarar 3.13.

**Fix (decidido, aplicado no `release.yml` job `build-macos`):** GDAL
**3.12.x do conda-forge** via micromamba — pin determinístico que casa com os
prebuilt-bindings `3_12` (e com o wrapper do gdal 0.19). Brew abandonado
(não existe `gdal@3.12` versionado no homebrew-core):

1. `mamba-org/setup-micromamba@4b9113af4fba0e9e1124b252dd6497a419e7396d` —
   SHA do **objeto da tag `v1`** consultado na API GitHub em 2026-10-17
   (`git/refs/tags/v1` → `4b9113af4fba0e9e1124b252dd6497a419e7396d`, type
   commit; registrado no comentário do YAML). `create-args`: `-c conda-forge
   gdal=3.12.* pkg-config` (pin de **minor**: qualquer 3.12.x casa com
   `prebuilt-bindings/3_12` — seleção por major.minor, build.rs:277 do
   gdal-sys).
2. Step "Exporta GDAL_HOME/PKG_CONFIG e assert versão 3.12.x": usa o output
   `environment-path` do action (prefixo real do env, sem depender de
   `CONDA_PREFIX`/`MICROMAMBA_ROOTPREFIX`), exporta `GDAL_HOME`,
   `PKG_CONFIG_PATH=$GDAL_HOME/lib/pkgconfig` e `PKG_CONFIG=$GDAL_HOME/bin/
   pkg-config`, valida existência de `gdal-config`/`gdal.pc` e **asserta**
   `pkg-config --modversion gdal` ∈ `3.12.*` (falha rápida com
   `::error::` se o solver entregar outra coisa). Contratos do build.rs do
   gdal-sys 0.12 verificados no fonte do registry (`~/.cargo/registry/src/
   …/gdal-sys-0.12.*/build.rs`): com `GDAL_HOME` setado o link sai de
   `$GDAL_HOME/lib` (`rustc-link-search`, metadata do pkg-config desligada)
   e o pkg-config serve só p/ detectar a versão → `PKG_CONFIG`+`PKG_CONFIG_
   PATH` explícitos tornam a detecção independente do PATH do runner.
3. Build sem bindgen: `cargo build --release -p ta-gui -p ta-cli` (a feature
   `ta-gis/bindgen-gdal` **fica** no `Cargo.toml` — útil quando o gdal-rs
   suportar 3.13+).
4. `packaging/macos/bundle.sh` adaptado (_dylib inspection real_: baixado o
   `libgdal-core-3.12.4` osx-arm64 do conda-forge — `LC_ID_DYLIB =
   @rpath/libgdal.38.dylib` e **32 deps todas `@rpath/`**; o antigo
   `case @*) continue` pularia tudo e o .app sairia quebrado):
   - novo `resolve_dep()`: caminho absoluto passa direto (brew); `@rpath/…`
     é resolvido contra (1) `LC_RPATH` do referenciador (novo
     `list_rpaths()` via `otool -l`), (2) `$GDAL_HOME/lib` (conda — todas as
     dylibs do env), (3) `/opt/homebrew/lib`, `/usr/local/lib`; não achou =
     **erro explícito** (nunca bundle pela metade);
   - `fix_deps()`: `-change` só em dependência absoluta; deps `@rpath/…`
     ficam como estão (basename em Frameworks é idêntico; dyld resolve via
     `LC_RPATH @executable_path/../Frameworks` do executável — vale p/ todo
     o link chain);
   - novo bloco final: copia `$GDAL_HOME/share/{proj,gdal}` →
     `Contents/MacOS/share/` (o `ta-gui/src/main.rs` — `use_bundled_gdal_
     data()` — aponta PROJ_LIB/GDAL_DATA p/ `<dir-do-exe>/share`; sem isso
     o .app abriria sem proj.db/dados GDAL);
   - re-assinatura ad-hoc já existente cobre as novas libs (todas passam por
     `install_name_tool`).
   Risco documentado no YAML: dylibs do conda + codesign ad-hoc — já
   tratado pelo re-sign; `@rpath` agora bate com a origem conda.

#### Causa 2 — Windows: unroll de pipeline do PowerShell corrompeu o ISCC_PATH

Descartado por evidência de log: **não** foi vcpkg/GDAL (caso (a) — o cargo
build completou e `GDAL_VERSION: 3.12.4`; bindgen no windows não é preciso)
nem regex do `vcpkg list` (caso (c) — a versão foi extraída corretamente).
Log do job (`jobs/117357839683/logs`):

- `ISCC real: C` (o `Find-ISCC` devolveu o **caractere** "C");
- env dump do step seguinte: `ISCC_PATH: C`;
- `Join-Path: ... Line | 25 | $langsDir = Join-Path (Split-Path
  $env:ISCC_PATH) "Languages" ... Cannot bind argument to parameter 'Path'
  because it is an empty string.` → `ErrorActionPreference='Stop'` → exit 1;
- o step durou 0,38 s (o caminho choco nem rodou) → a origem foi o ramo
  rápido `%ProgramFiles(x86)%` (Inno Setup 6 **é** pré-instalado no runner).

Root cause: `@(…, …) | Where-Object {…}` com **exatamente 1 match** sofre
*unroll* — `$std` vira **string** (não array) e `$std[0]` indexa o 1º
CARACTERE de `C:\Program Files (x86)\Inno Setup 6\ISCC.exe` → `"C"`.

**Fix (step "Localiza ISCC.exe real"):** `Select-Object -First 1` fecha o
pipeline e `return $std` sem indexação (escalar OU array ok, mesmo nos
ramos registry/choco); asserção `Test-Path $isccPath` antes de exportar
`GITHUB_ENV` transforma qualquer regressão futura em erro explícito no step
certo (em vez de `ISCC_PATH` podre explodindo 2 steps adiante).

#### Verificação local (host Linux, 2026-10-17)

| Check | Resultado |
|-------|-----------|
| pyyaml `release.yml` + `ci.yml` | ✅ (jobs: 5 e 3, estrutura íntegra) |
| `bash -n packaging/macos/bundle.sh` | ✅ |
| `cargo build --workspace --offline` | ✅ 1,7s (cache quente) |
| Windows caso (a)? | ❌ **não se aplica** — o build cargo passou no CI; a falha era o ISCC (caso (b), causa nova). Sem necessidade de re-prova do `bindgen-gdal` no windows |
| Sintaxe PowerShell dos steps Windows | ⚠️ revisão manual (pwsh não existe no host; pyyaml valida o YAML) |

Limitação honesta: os caminhos macOS (micromamba/conda) e o resolver de
`@rpath` do bundle.sh rodam **só em runners macOS** — a prova real é a run
#5. No host Linux verificou-se: YAML, sintaxe bash, build do workspace e os
contratos do gdal-sys 0.12 + a dylib conda real (inspecionada aqui mesmo).

#### Arquivos p/ @iris (commit único, NÃO pushado ainda)

1. `.github/workflows/release.yml` — job macOS (micromamba/conda-forge 3.12,
   sem bindgen, asserts) + fix ISCC do windows (unroll + asserção)
2. `packaging/macos/bundle.sh` — resolver `@rpath`/origem-agnóstico + dados
   `share/{proj,gdal}` no bundle
3. `.pantheon/deepwork/terraamazon-cleanroom-rust/STATUS.md` — esta seção
   (run #4)

### Run #5 — release (tag v0.1.4-alpha, commit `0c295fa`, 2026-10-17): macOS falhou no codesign final

| Job | Resultado |
|-----|-----------|
| `build-linux` | ✅ verde (tar.gz + .deb) |
| `build-windows` | ✅ verde (zip + instalador Inno Setup — fix ISCC da run #4 confirmado) |
| `sbom` | ✅ verde |
| `build-macos` | ❌ step "Bundle CurupiraGIS.app + zip" (job `117363584508`) |
| `release` (draft) | ⏭️ pulado (`needs` reprovado) |

#### Causa raiz — codesign: dados do PROJ/GDAL sob Contents/MacOS

Log do job: o bundle montou tudo (36 dylibs re-assinadas, binários com
`@rpath` reescrito, `dados bundled: .../MacOS/share/proj|gdal` copiados) e o
sign do executável principal explodiu:

```
dist/CurupiraGIS.app/Contents/MacOS/curupira-gis: replacing existing signature
dist/CurupiraGIS.app/Contents/MacOS/curupira-gis: code object is not signed at all
In subcomponent: .../CurupiraGIS.app/Contents/MacOS/share/proj/nad83
##[error]Process completed with exit code 1.
```

Mecânica: ao assinar o executável de `Contents/MacOS` com `Info.plist` irmão,
o codesign enumera o código aninhado do bundle — e **TODO arquivo sob
`Contents/MacOS` é tratado como código**. Os dados do PROJ/GDAL copiados para
`MacOS/share` (decisão da run #4) viraram "subcomponentes" não-assinados →
`code object is not signed at all`. O sign do `ta-cli` (linha seguinte do
script) nunca rodou — o `set -e` matou o step na re-assinatura do `curupira-gis`.

**Fix (2 partes acopladas):**

1. `packaging/macos/bundle.sh`:
   - dados `share/{proj,gdal}` migram de `Contents/MacOS/share` →
     `Contents/Resources/share` (padrão .app: código em MacOS/, dados em
     Resources; Resources é selado no CodeResources como recurso — sem
     assinatura);
   - ordem de assinatura explícita: (a) cada dylib de Frameworks (ad-hoc,
     já existia), (b) `ta-cli`, (c) `curupira-gis` (main exe é o ÚLTIMO
     código assinado — a enumeração disparada no sign dele só encontra
     assinaturas válidas), (d) POR ÚLTIMO o bundle inteiro **sem `--deep`**
     (`codesign --force --sign - dist/CurupiraGIS.app` — sela Resources e
     valida o aninhado já assinado; `--deep` não existia no script e segue
     proibido: re-assinaria tudo e mascararia regressão de ordem).
2. `crates/ta-gui/src/main.rs` — `use_bundled_gdal_data()`: mantém
   `<exe_dir>/share/{proj,gdal}` como 1ª escolha (Windows `.iss` instala em
   `{app}\share`; tar.gz Linux ao lado do binário — **intactos**) e adiciona
   fallback `<exe_dir>/../Resources/share/{proj,gdal}` para o bundle macOS.
   Resolver extraído p/ `bundled_data_dir()` + teste unitário novo
   (`resolver_prefere_share_e_cai_para_resources` — prova as duas bases).

#### Verificação local (host Linux, 2026-10-17)

| Check | Resultado |
|-------|-----------|
| `bash -n packaging/macos/bundle.sh` | ✅ |
| pyyaml `release.yml` (comentário atualizado p/ Resources) | ✅ |
| `cargo build --workspace` | ✅ |
| `cargo test --workspace` (RUSTFLAGS=-D warnings) | ✅ 137 passed / 0 failed (inclui o novo teste do resolver) |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ |
| `cargo fmt -p ta-gui -- --check` | ✅ |

Limitação honesta: codesign/enumeração de bundle só existem em runner macOS —
a prova real é a run #6.

#### O que a run #6 vai provar (simulação mental, evidências da run #5)

1. `dados bundled: .../Contents/Resources/share/proj|gdal` no log (antes:
   `.../MacOS/share/proj|gdal`) — nenhum dado sob MacOS/.
2. Sequência de signs sem erro: 36× dylibs → `ta-cli` → `curupira-gis` →
   `CurupiraGIS.app` (a run #5 morreu ANTES do sign do `ta-cli`; se a
   enumeração do main exe achar só assinaturas válidas + Resources selados,
   passa).
3. Zip `CurupiraGIS-macos-arm64.zip` com `Contents/_CodeSignature/CodeResources`
   (novo — o sign do bundle cria) e sem `Contents/MacOS/share`.
4. Draft release criado com os 5 entregáveis (macOS era o único faltante —
   windows/linux/sbom já verdes na run #5 e os jobs não dependem do fix).

#### Arquivos p/ @iris (commit único, NÃO pushado ainda; tag sugerida `v0.1.5-alpha`)

1. `packaging/macos/bundle.sh` — dados em Resources + ordem de codesign sem `--deep`
2. `crates/ta-gui/src/main.rs` — fallback `../Resources/share` + teste do resolver
3. `.github/workflows/release.yml` — só comentário (documenta o destino Resources)
4. `.pantheon/deepwork/terraamazon-cleanroom-rust/STATUS.md` — esta seção (run #5)

Push em `main` + tag `v0.1.5-alpha` → dispara a run #6 (workflow: `on: push: tags: ["v*"]`).




---

## Fase v0.2 — UI PARITY (@aphrodite, 2026-10-17)

Referência de layout: screenshot do TerraAmazon 7.3.2 (menu denso, duas
toolbars agrupadas, trilho de ícones + painel "Camadas", canvas grande,
modal "Bem Vindo" com 4 cards de persona, status bar com seleção/SRS/Scale).
**Clean-room mantido**: nenhum código/asset do binário foi consultado ou
copiado — ícones são glifos unicode e desenhos próprios no painter do egui;
todos os textos são próprios (i18n pt-BR/en).

### Entregas

| Item | Entregue |
|---|---|
| **Diálogo Bem-vindo (persona picker)** | ✅ modal central na 1ª abertura (`crates/ta-gui/src/welcome.rs`): título + prompt + 4 cards clicáveis (**SIG**/**Assistente**/**Operador**/**Administrador**) com ícones desenhados no painter (globo, prancheta, lápis, escudo — zero asset binário), botão "Ajuda" (abre Sobre com link do repo) e checkbox "Exibir esta janela na abertura" |
| **Personas = presets de feature flags** | ✅ `persona.rs`: `PersonaFlags` (inspector/attr_table/editing/import_export/project_mgmt/tools/processing/admin/plugins/wizard) aplicados ao `AppState` (novo `apply_persona`). SIG = tudo; Assistente = SIG + wizard; Operador = mapa+camadas+import (sem admin/gerência/processamento); Administrador = conexão/admin/migrações (sem edição). **O canvas nunca é escondido** (contrato testado) |
| **Persistência do workbench** | ✅ `workbench.json` em `$XDG_CONFIG_HOME/curupira/` ou `$HOME/.config/curupira/` (std puro, sem crate nova): persona + show_welcome + idioma. Arquivo ausente/corrompido/persona inválida → defaults (SIG + bem-vindo), **nunca panic**. Caminho resolvido 1× em `TerraApp::new` (`workbench_path`) — testes injetam caminho temporário sem tocar env |
| **Menu bar completa** | ✅ `menus.rs`: Arquivo (novo/abrir projeto, importar, **exportar GeoJSON da camada ativa**, sair) · Editar (undo/redo attrs, deletar feição) · Área de Trabalho (personas, reabrir bem-vindo, tema, idioma) · Mapa (zoom, extensão, **grade**, **barra de escala**) · Ferramentas (conectar/desconectar, **console SQL somente-leitura**, opções) · Gerenciamento de Projeto (criar/**renomear**/**deletar** projeto, **renomear**/remover camada) · Administração (**aplicar migrações**, **info do banco** — só admin) · Plugins (stub informativo) · Processamento (**buffer/interseção/união PostGIS**) · Ajuda (atalhos, sobre) |
| **Explorador de camadas upgraded** | ✅ ícone por `geom_type` (●─▲◈▦), context menu (zoom para camada, renomear [banco], remover, **propriedades**: nome/geometria/CRS/contagem/origem), checkbox de visibilidade, ordem ▲▼, camada ativa; **trilho de ícones vertical** à esquerda (importar, conectar, extent, identificar/novo ponto — toggles de modo) |
| **Status bar completa** | ✅ `status_bar.rs`: `Linhas selecionadas: N` (bind real), chip SRS (verde `EPSG:4674` com projeto / vermelho "SRS desconhecido" sem — distingue por TEXTO, a11y), coordenadas do cursor, **dropdown Scale** com presets 1:1k…1:5M (aplicam `px_per_unit` via inverso exato de `scale_denominator`), modo LOCAL/BANCO, engrenagem → Opções |
| **Toolbars agrupadas** | ✅ duas linhas densas com separadores (arquivo/projeto · import/export · conexão · zoom · overlays · modos de mouse · undo/redo/delete) — mesmas ações dos menus, tooltips i18n, filtradas por persona |
| **Overlays do canvas** | ✅ `overlays.rs` (puro, testado): grade de coordenadas 1-2-5 (~80 px) desenhada sobre o mapa sem tocar no contrato MapView; barra de escala (metros→px, rótulo m/km) |
| **Console SQL** | ✅ somente-leitura: guard `is_read_only_sql` na UI **e** no worker (SELECT/WITH, sem `;`), resultado via `row_to_json` limitado a 200 linhas, `AssertSqlSafe` justificado (mesmo padrão do CLI) |
| **ta-db estendido (v0.2)** | ✅ `repo.rs`: `ProjectRepo::rename` / `LayerRepo::rename` (bind-only, seguem padrão existente); **novo** `ops.rs`: buffer/interseção/união **server-side** criando camadas derivadas (INSERT…SELECT PostGIS, attrs de rastreabilidade, bbox recalculado) — **tudo em uma transação** (`pool.begin()`/`commit`, rollback em qualquer erro SQL ou resultado vazio): camada derivada nunca fica órfã. Correção do gate themis: `union` agregava `ST_Union` com coluna solta (`f.layer_id`) — SQL inválido (PG 42803); agora attrs usam `min(f.layer_id::text)` (fonte única por chamada). Interseção computa `ST_Intersection` uma única vez (CTE). ⚠️ pequeno acréscimo de backend feito no escopo da fase — gate @hermes/@themis: 2 high corrigidos e provados no PG real |
| **i18n** | ✅ 168 chaves (89 → 168): menus novos, personas, bem-vindo, wizard, processamento, console, gerência de projeto, overlays, rail — 100% dos rótulos; teste de completude atualizado |
| **Testes** | ✅ +20 (137 → **157**): persona (5 unit + smoke), workbench (resolver/roundtrip/corrompido/default), overlays (presets/grade/barra), `is_read_only_sql`, export GeoJSON end-to-end, smoke v0.2 headless (bem-vindo+personas renderizando, flags por persona, canvas sempre presente, escala por preset, workbench persistindo com caminho injetado, ações sem banco falham suave), ícones de geometria distintos, rename+ops gated no `pg_integration.rs` |

### Verificação executada (host Linux, 2026-10-17)

| Check | Resultado |
|---|---|
| `cargo build --workspace` | ✅ verde |
| `cargo test --workspace` | ✅ **157 passed / 0 failed** (gated PG compila e pula sem banco) |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde |
| `cargo fmt --all -- --check` | ✅ verde |
| Smoke visual/GPU | ⚠️ host sem display — checklist v0.2 adicionado em `benches/map_render.md` |

### Riscos honestos / limitações

1. **Só roda com display (não provável aqui)**: cards do bem-vindo clicáveis de verdade (hit-test egui), menus abrindo/fechando, tooltips, context menu com botão direito, dropdown de escala interagindo, grade/barra desenhadas em GPU — tudo headless-testado no nível "renderiza sem panic + lógica pura", mas a interação real precisa do checklist novo em `benches/map_render.md`.
2. **Processamento exige banco real**: as ops PostGIS só executam conectado+projeto (desenho intencional); o teste é **gated** (`TA_TEST_PG=1`) — a prova SQL real contra PostGIS 3.4 acontece no job `linux-pg` do CI ou com `scripts/test-pg.sh`.
3. **Console SQL é somente-leitura por design** (SELECT/WITH, sem `;`, 200 linhas); `EXPLAIN` puro fica fora (não é envolvível em `row_to_json`) — documentado no hint do diálogo.
4. **Escalas são aproximação geográfica** (herdado do viewport: 1° ≈ 111 320 m no equador) — presets herdam essa precisão.
5. **Menú "Controladores" da referência não foi reproduzido** (sem equivalente funcional no escopo v0.2); Plugins é stub honesto informativo.
6. **Tema não é persistido** no workbench.json (preferência do egui é runtime do contexto); persona/idioma/bem-vindo sim.
7. **Renomear camada/projeto exige conexão** (o rename é persistido no banco); camadas locais renomeiam apenas removendo/reimportando (limitação MVP documentada).

### Não commitado

Nada foi commitado (instrução da subtask). Arquivos novos: `persona.rs`, `workbench.rs`, `overlays.rs`, `menus.rs`, `status_bar.rs`, `welcome.rs`, `work_dialogs.rs`, `export_job.rs` (ta-gui), `ops.rs` (ta-db). Modificados: `app.rs`, `app_state.rs`, `panels.rs`, `dialogs.rs`, `db_worker.rs`, `map_view.rs`, `i18n.rs`, `lib.rs`, `Cargo.toml` (ta-gui: +serde, +feature geojson), `repo.rs`/`lib.rs` (ta-db), `tests/app_smoke.rs`, `tests/pg_integration.rs`, `benches/map_render.md`, este STATUS.md.

---

## Fase v0.2.1 — NAVEGAÇÃO E ABERTURA DE ARQUIVOS DE 1ª CLASSE (@aphrodite, 2026-10-17)

Pedido do usuário: busca de arquivos melhor, janela de menu para navegar/mapear
caminho, mesma UX em Windows e Linux, acessível pelo menu. **Nota honesta**: a
premissa "hoje depende do rfd" não se confirmou — `rfd` nunca esteve no grafo
(o diálogo de import sempre foi um campo de texto). Restrição "se rfd já está,
mantenha" → não havia nada para manter; **nenhuma dependência nova** foi
adicionada (walk é std com depth limit; nada de scanlex/jwalk).

### Entregas

| Item | Entregue |
|---|---|
| **File browser embutido** | ✅ novo `file_browser.rs` (egui-native, módulo próprio): modal "Navegador de arquivos…" acessível por **4 caminhos** — menu Arquivo → "Navegador de arquivos… (Ctrl+Shift+O)", botão 🗂 na toolbar de arquivo, atalho **Ctrl+Shift+O** e botão no diálogo de importação (toggle "usar navegador embutido"). Breadcrumb clicável + ↑ + Home + Projeto (pasta da origem da camada ativa) + "Dados de teste" (testdata do repo ao lado do binário — **skip silencioso** quando ausente, p.ex. instalador). Campo de caminho editável: Enter navega, dir → navega, arquivo → seleciona no pai, inexistente → **erro inline vermelho** (sem panic). Listagem: pastas primeiro, nome case-insensitive, ícone por tipo (shp/geojson/gpkg/fgb/tif/csv reconhecidos pela extensão), **duplo clique entra/importa** |
| **Busca/filtro** | ✅ substring case-insensitive na pasta corrente; toggle "Buscar em subpastas" sobe thread própria (`spawn_walk`, cancelável via `AtomicBool`, **depth 3**, teto 500 resultados, spinner + Cancelar no loading state); **F3** pula entre ocorrências |
| **Painel de detalhes** | ✅ lateral direita: nome, tamanho (formato por idioma: vírgula pt-BR / ponto en), última modificação (`civil_from_days` de Hinnant — sem crate de data) e **formato detectado via `ta_gis::detect::detect_source_format`** (extensão + magic bytes, API já pública; fallback GeoTIFF/extensão) |
| **Import pelo browser** | ✅ botão "Importar" habilitado só para formatos suportados → **fluxo único consolidado** `TerraApp::start_import_from_path` (valida → `spawn_import` → normalizer → `bulk_insert`/camada local em background). O diálogo de importação antigo passou a usar o MESMO método (um só caminho para cada efeito) |
| **Recentes** | ✅ `workbench.json` ganha `recent_files` (máx. **15**) e `recent_dirs` (máx. **8**), com timestamp Unix; `#[serde(default)]` — workbench da v0.2 abre normal. Atualiza em todo import/export (arquivo **e** sua pasta) e navegação no browser. Menu Arquivo → **"Abrir recente ▸"** (lista clicável; arquivo que sumiu do disco é **removido on-click** e persistido); seção "Recentes" no painel de detalhes do browser |
| **Drag & drop** | ✅ soltar `.shp/.geojson/.gpkg/.fgb/.tif/.csv` do SO na janela (egui raw `dropped_files`) abre o modal de importação **pré-preenchido** com o 1º (sem auto-start — usuário confirma o SRID); multi-arquivo → **fila** (`import_queue`) com hint "+N na fila" no diálogo, consumida em sequência ao confirmar (`import_queue_auto`); cancelar descarta a fila; extensão não suportada → erro, nada abre |
| **Paridade Win/Linux** | ✅ o browser é 100% egui — idêntico por construção (é o ponto do pedido). Breadcrumb mostra `/` ou `C:\` (`path_anchor` via components, sem `Path::anchor`); `~` expande com HOME/USERPROFILE (puro, testável). **rfd**: inexistente no grafo, portanto "Usar diálogo do sistema" não existe para manter — o navegador embutido passou a ser o caminho primário e o campo de texto manual continua como opção universal (documentado como decisão, não como regressão) |
| **UX polish** | ✅ teclado: **Esc fecha**, **Ctrl+F** foca o filtro, **Alt+↑** sobe, **↑/↓** movem a seleção (com wrap; campo de texto focado vence — a11y), **Enter** abre 1º resultado/F3 próximo, **F3** próxima ocorrência; loading state no walk (spinner + cancelar); import concluído registra recentes |
| **i18n** | ✅ 168 → **191 chaves** (+23: browser completo, recentes, fila, erros de caminho/drop sem suporte) — pt-BR/en exaustivos, teste de contagem atualizado |
| **Testes** | ✅ +20 (157 → **177**, todos verdes): classificação por extensão, `list_dir` (ordenação/metadados, tmpdir), filtro case-insensitive, walk depth/cancel/teto/ordem estável, `validate_path` + `expand_tilde`, testdata resolver, `project_dir`, `format_size` (pt/en), `format_mtime` (datas conhecidas), `plan_drop`, recents (dedupe/trim 15/8/remoção/serde-default), smoke headless: browser abre/navega/fecha, drag-drop prefill+cancel, **fila multi-arquivo avançando de verdade** (2 imports end-to-end até camada), validação de import + recente inválido, recents persistindo no JSON |
| **STATUS/checklist** | ✅ esta seção + `benches/map_render.md` ganhou checklist manual v0.2.1 (10 itens) |

### Verificação executada (host Linux, 2026-10-17)

| Check | Resultado |
|---|---|
| `cargo build --workspace` | ✅ verde |
| `cargo test --workspace` | ✅ **177 passed / 0 failed** (157 + 20 novos; gated PG compila e pula sem banco) |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde (2 acionados e corrigidos: `sort_by_key`, `format!` inútil) |
| `cargo fmt --all -- --check` | ✅ verde |
| Smoke visual/GPU | ⚠️ host sem display — checklist v0.2.1 adicionado em `benches/map_render.md` |

### Decisões de design (registradas)

1. **Fluxo de import único**: `start_import_from_path` é o único caminho que sobe import (diálogo, browser, recentes, drop) — elimina a duplicação diálogo/app e dá um ponto só de validação/erro.
2. **Recents em import/export registram também a pasta**: alimenta recent_dirs e o atalho "Projeto" mesmo sem camada no banco.
3. **Drop não auto-importa**: pré-preenche o modal (SRID visível, usuário confirma) — multi-arquivo vira fila confirmada; cancelar limpa tudo. Falha no meio da fila interrompe e limpa.
4. **Walk em thread com lote único + sort determinístico** (nome case-insensitive): resultados idênticos entre sessões — F3 e testes dependem disso.
5. **`format_size`/`format_mtime` próprios**: separador decimal por idioma e ISO curto UTC — sem crate nova (regra "sem deps pesadas").
6. **Smoke de import agora usa `workbench_path = None`**: evita poluir o `~/.config/curupira/workbench.json` real do host com caminhos de fixture (efeito colateral dos recents; corrigido no teste pré-existente).

### Riscos honestos / limitações

1. **Listagem é síncrona** (um `read_dir` por navegação): OK para discos locais; rede lenta pode travar um frame (limitação documentada — o caminho pesado, busca em subpastas, é em thread).
2. **Busca em subpastas importa só arquivos**: diretórios correspondentes não aparecem nos resultados (padrão de "find de arquivos").
3. **F3/Enter-no-walk** operam sobre o painel de resultados corrente; não há histórico de buscas (YAGNI).
4. **`~` no Windows** só expande com USERPROFILE; `~user` não é resolvido (documentado).
5. **Seleção única** no browser (sem multi-select com Ctrl) — a fila multi-arquivo vem do drag & drop; multi-select no browser fica para feedback.
6. **Interação real de janela** (modal sobre canvas, foco Ctrl+F, duplo clique) continua impossível de provar headless — coberta pelo checklist manual novo.

### Não commitado

Nada foi commitado (instrução da subtask). Arquivo novo: `file_browser.rs` (ta-gui). Modificados: `app.rs` (browser/fila/drop/start_import/recents/atalho), `workbench.rs` (RecentItem/recent_files/recent_dirs/métodos), `menus.rs` (item de menu, "Abrir recente ▸", botão toolbar), `dialogs.rs` (dispatcher do browser, import usa fluxo único + fila + botão browser), `i18n.rs` (+23 chaves), `lib.rs` (módulo), `tests/app_smoke.rs` (+5 smoke v0.2.1), `benches/map_render.md` (checklist), este STATUS.md.

### Correções do gate themis (v0.2 — @hermes, 2026-10-17)

O gate bloqueou v0.2 com 2 high em `crates/ta-db/src/ops.rs` (ambos provados no PG real pelo gate):

1. **`ops::union` — SQL inválido (PG 42803)**: `jsonb_build_object('source', f.layer_id::text)` misturava coluna não-agregada com `ST_Union(f.geom)`. Corrigido para `min(f.layer_id::text)` — o `WHERE` prende uma única camada fonte por chamada, então o agregado registra exatamente a origem. Regressão no teste gated: attrs `source` da união == id da camada fonte.
2. **Ops não transacionais — camada derivada órfã**: `create_derived_layer` + `INSERT…SELECT` + limpeza não eram atômicos; erro SQL depois do registro deixava camada com 0 features (bypass do DELETE manual de limpeza). Todas as 3 ops (buffer/interseção/união) agora rodam em `pool.begin()`…`commit()` (`rollback` no erro/vazio — o DELETE manual deixou de existir); `PgLayerRepo` ganhou `add_on`/`update_bbox_on` (executor genérico) para compor a transação. Regressão no teste gated: erro SQL real (cast `$2::uuid` falhando após criar a camada) → erro E nenhuma camada órfã.

Correção de documentação: "i18n.rs (+22 chaves)" acima era inconsistente com o total correto (**191**, 168 → 191 = **+23**, coberto pelo teste de contagem). Limitação do console SQL documentada em `db_worker.rs`: a guard `is_read_only_sql` é string-matching e `WITH … INSERT` em instrução única passa nela (bypass conhecido; mitigação real — transação `READ ONLY` — ficou para o backlog).

---

## Fase v0.2.2 — REGISTRO DE COMANDOS: PALETA (Ctrl+K) + CAIXA DE PROCESSAMENTO (@aphrodite, 2026-10-17)

Padrões de referência (pesquisa da subtask): **RustGIS** (github opengeos/RustGIS,
Apache-2.0) tem como pilar "toda ação é um comando scriptável"; **QGIS** tem a
caixa de ferramentas de processamento com busca (toolbox dockável + palette
Ctrl+K). Clean-room mantido: nenhuma linha/asset copiado — apenas o padrão de
UX; **nenhuma dependência nova** (fuzzy match é implementação própria ~30
linhas em `command_registry.rs`).

### Entregas

| Item | Entregue |
|---|---|
| **Command Registry (fonte única)** | ✅ novo `command_registry.rs`: `Command {id, label(i18n), category, shortcut, sep, visible, guard, toggle, run}` — **44 comandos** cobrindo TODA ação de menu/toolbar existente (arquivo, editar, área de trabalho incl. personas/tema/idioma, mapa incl. modos, ferramentas, gerência de projeto, administração, plugins, processamento, ajuda). `run: fn(&mut TerraApp, &Context)`; `guard: fn(&TerraApp) -> Option<chave_i18n>` devolve o **motivo** da desabilitação; `visible` filtra por persona (mesmos flags dos menus). Ids estáveis (`"file.import"`, `"processing.buffer"`…) — a base scriptável do padrão RustGIS |
| **Menus/toolbars consomem o registro** | ✅ `menus.rs` reescrito: os 10 menus renderizam do registro (`menu_commands`), toolbars idem (`reg_toolbar`/`reg_toolbar_toggle` acionam `run` por id) — **zero comportamento duplicado**: menu, toolbar, paleta e caixa de ferramentas são 4 vistas do MESMO comando. Submenus com marcador (personas/tema/idioma) mantêm a apresentação, mas a ação continua sendo o `run` do registro |
| **Paleta de comandos (Ctrl+K)** | ✅ novo `palette.rs`: modal com campo de busca focado, **fuzzy match próprio** (subsequência case-insensitive; ranking por posição — consecutivas e início cedo vencem; casa rótulo i18n + categoria com penalidade), ↑/↓ navegam com wrap, Enter executa, Esc/backdrop fecha; cada linha mostra categoria + atalho; ações desabilitadas ficam esmaecidas **com o motivo no hover** (a11y). Aberta por **Ctrl+K** (qualquer foco), menu Área de Trabalho → "Paleta de comandos… (Ctrl+K)" e pelo próprio registro (`workspace.palette`) |
| **Caixa de processamento (padrão QGIS)** | ✅ novo `toolbox.rs`: janela flutuante/redimensionável (dockável sem crate de docking) com **busca fuzzy** e catálogo `ProcessingTool {id, label, description, params_schema, command}` — hoje: Buffer, Interseção, União (ops PostGIS) + Importar + Exportar. Clicar executa o **comando do registro** → abre o diálogo de parâmetros existente pré-preenchido (ex.: Buffer → diálogo com sufixo `_buffer`). Guardas vêm do comando (sem banco → esmaecida com motivo). Aberta por menu Área de Trabalho, paleta e `workspace.toolbox` |
| **Extensibilidade (prepara plugin system)** | ✅ documentado no header de `toolbox.rs` (3 passos: Command no registro → ProcessingTool no catálogo → chaves i18n) e teste de integridade: toda tool aponta para comando existente e **todo comando de Processamento tem tool** — a caixa nunca lista ferramenta morta, e plugin novo aparece em menu+paleta+toolbox de graça |
| **Polish de toolbar** | ✅ tooltips com atalho real: 📂 "Abrir projeto (Ctrl+O)", 📥 "Importar camada (Ctrl+I)", 🗂 navegador (Ctrl+Shift+O, existente); toggles de modo/overlay (✛ identificar, 📍 novo ponto, ⌗ grade, ▭ barra) agora usam `toggle_value` — **mesmo estado hover/ativo** para todos (antes era misto toggle/selectable) |
| **Título da janela** | ✅ "Curupira GIS — {projeto}" com projeto aberto (dados dinâmicos via `format!`, rótulo via i18n); `ViewportCommand::Title` enviado só quando muda (`last_title` evita comando por frame) |
| **i18n** | ✅ 191 → **214 chaves** (+23: paleta, caixa de ferramentas, descrições das 5 tools, tooltips com atalho, 6 motivos de desabilitação, 2 idiomas) — pt-BR/en exaustivos, teste de contagem atualizado |
| **Testes** | ✅ +14 (177 → **191**, todos verdes): registry — ids únicos, fuzzy (não-casa → None, consecutivo/bundo cedo vencem, case-insensitive, vazia casa tudo), busca ranqueada e estável, guardas/visibilidade por persona (Operador/Admin/SIG + paleta esconde invisíveis + undo habilita após edição), toggles refletem estado, run de TODOS os 44 comandos sem panic; toolbox — ids únicos, integridade tool→comando e cobertura de Processamento, busca (vazio/buf/inter/xyz), schemas com parâmetros mínimos; smoke — **Ctrl+K real abre/fecha a paleta** (eventos `ModifiersChanged`+`Key` no ctx headless), todos os comandos renderizam após `run`, toolbox abre/busca/executa pré-preenchendo o diálogo, título com projeto |

### Verificação executada (host Linux, 2026-10-17)

| Check | Resultado |
|---|---|
| `cargo build --workspace` | ✅ verde |
| `cargo test --workspace` | ✅ **191 passed / 0 failed** (177 + 14 novos; gated PG compila e pula sem banco) |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde |
| `cargo fmt --all -- --check` | ✅ verde |
| Smoke visual/GPU | ⚠️ host sem display — checklist v0.2.2 adicionado em `benches/map_render.md` |

### Decisões de design (registradas)

1. **Comportamento mora só no registro**: menu, toolbar, paleta e caixa de ferramentas são vistas do mesmo `Command` — o anti-requisito "sem duplicar" é estrutural (uma ação nova é registrada UMA vez). Testes de integridade barram deriva.
2. **`guard` devolve motivo i18n** (não booleano): a paleta mostra "Conecte-se a um banco de dados" na ação esmaecida — polimento QGIS/VS Code e a11y (não depende só de cor). Motivos compostos (banco → projeto) encadeiam `or_else`.
3. **Fuzzy próprio (~30 linhas)**: subsequência case-insensitive com custo = posição inicial + buracos (consecutivas custam 0); empate → ordem do registro. Sem crate nova (regra da subtask); substituível por algo tipo nucleo/nu-AnsiHighlight quando plugin system exigir.
4. **Personas/tema/idioma como comandos**: paleta cobre troca de persona, tema e idioma (QGIS faz igual); o menu mantém submenus com marcador da corrente, mas a ação é o `run` do registro.
5. **Toolbox = `egui::Window`** (flutuante/redimensionável), não modal: processamento QGIS é dock que convive com o mapa; a crate de docking fica para quando houver mais docks (YAGNI).
6. **Atalho real ≠ atalho exibido**: egui não tem aceleradores nativos; o disparo continua em `handle_shortcuts` (Ctrl+K adicionado) e o registro guarda a string para tooltip/paleta.
7. **Ctrl+K funciona com campo focado** (não é gated por `egui_wants_keyboard_input`): paleta é navegação, como Ctrl+O/Ctrl+I.

### Riscos honestos / limitações

1. **Interação real de paleta (digitar/filtrar/setas sobre o modal em GPU)** não é provável headless — o smoke testa Ctrl+K real, renderização, busca, seleção e execução por caminhos de código; digitação/setas/Enter na janela real ficam no checklist manual novo.
2. **Dois comandos de idioma com rótulos distintos** (`LangPtBr`/`LangEn`) foram criados para a paleta; o submenu de idioma continua mostrando `display_name()` (apresentação) — os dois caminhos executam o mesmo `set_language`.
3. **Modo local**: processamento e gerência de projeto aparecem desabilitados com motivo na paleta (e ocultos nos menus por persona, como antes) — usuário Operador/SIG em modo local enxerga na paleta o que falta para habilitar (escolha de design, alinhada com o hint do menu Processamento).
4. **`params_schema` é documental** nesta fase: o diálogo de parâmetros continua sendo o existente (pré-preenchido pelo comando); schema validado/gerado fica para o plugin system.
5. **Sem histórico na paleta** (comandos recentes/mais usados) — YAGNI para v0.2.2; o ranking por posição já dá UX boa com 44 comandos.

### Não commitado

Nada foi commitado (instrução da subtask). Arquivos novos: `command_registry.rs`, `palette.rs`, `toolbox.rs` (ta-gui). Modificados: `menus.rs` (consome o registro — toolbars e menus sem comportamento próprio), `app.rs` (estado da paleta/toolbox + Ctrl+K + título da janela), `dialogs.rs` (dispatcher da paleta/caixa), `i18n.rs` (+23 chaves), `lib.rs` (módulos), `tests/app_smoke.rs` (+4 smoke v0.2.2), `benches/map_render.md` (checklist v0.2.2), este STATUS.md.

## Fix CI run v0.2.2 — `cargo deny` advisories zbus/zvariant (@prometheus, 2026-10-17)

Run [39048582185](https://api.github.com/repos/ils15/curupira-gis/actions/runs/39048582185) (push v0.2.2): falhou SÓ o job `linux (fmt + clippy + test, sem PG)`; linux-pg, windows e release passaram.

### Causa raiz (log real, job 117442233850)

- `cargo fmt --check`, `cargo clippy -D warnings` e `cargo test --workspace` **passaram** (toolchain CI: rustc 1.99.0; local: 1.99.1 — sem drift relevante).
- A falha foi a etapa **`cargo deny check licenses advisories bans sources`**: `advisories FAILED` com 3 advisories **INFO/unmaintained** emitidos em 2026-10-17 (véspera da run):
  - RUSTSEC-2026-0370 (`zvariant`), RUSTSEC-2026-0371 (`zbus_names`), RUSTSEC-2026-0372 (`zvariant_derive`) — "no patched versions"; `zvariant` foi absorvida pelo **zbus 6.0.0**.
- Cadeia transitiva: `ta-gui → eframe 0.37 → egui-winit 0.37 → accesskit_unix 0.24 → atspi → zbus 5.19 → zvariant 5.15`. O consumidor `accesskit_unix 0.24` ainda requer zbus 5.x → **sem rota de upgrade** (`cargo update` não cruza major incompatível). O `cargo deny` só roda no job linux (ci.yml:47) — por isso os outros jobs passaram.
- Suspeitas descartadas com evidência: fmt ✅, clippy ✅, testes ✅ no log; job linux **instala** `libgdal-dev` (ci.yml step `apt-get install -y libgdal-dev`) — GDAL não era o problema.

### Fix aplicado (mínimo)

- `deny.toml` → `[advisories] ignore`: +3 entradas documentadas (mesmo padrão já usado para RUSTSEC-2026-0192/ttf-parser), com motivo e condição de remoção (quando egui/accesskit migrar para zbus 6).

### Verificação local (replica comandos exatos do yml, host Linux)

| Check | Resultado |
|---|---|
| `cargo deny check licenses advisories bans sources` | ✅ `advisories ok, bans ok, licenses ok, sources ok` |
| `cargo fmt --all -- --check` | ✅ verde |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ verde |
| `cargo test --workspace` | ✅ 191 passed / 0 failed (contagens idênticas ao log do CI) |

### Não commitado

Nada commitado (instrução). Modificado: `deny.toml`, este STATUS.md. Arquivos para @iris: `deny.toml` (commit no push de fix).

---

## Re-planejamento — PLANO DE PARIDADE DO FLUXO DE MONITORAMENTO (@mnemosyne, registro em 2026-10-18)

Plano **aprovado pelo usuário** registrado como artefato em
`PLAN-terraamazon-parity.md` (mesma pasta). Este é um **re-planejamento
domain-first**; não substitui nem reescreve o histórico v0.1–v0.2.2 acima.

### Resumo do que foi aprovado

- **Objetivo:** Curupira GIS → réplica funcional **clean-room** do **fluxo de
  monitoramento** do TerraAmazon 7.3.2 (paridade **comportamental**, não de código).
- **Diagnóstico raiz (medido):** GUI-first — `ta-gui` **12.270** linhas `.rs`
  (src+tests) sobre domínio raso (`ta-core` **419** linhas; **3 tabelas**:
  `projects`/`layers`/`features`) ⇒ a paridade exige abordagem **domain-first**.
- **Escopo IN:** projeto/fase/AOI/células bloqueadas/classes/regras-rulesets/fila
  de processos/classificação básica/área/import-export/visualização.
  **OUT:** raster pesado, DTM, gráficos, layout de impressão, servidor WMS/WCS,
  geocoding, plugin manager real, multi-usuário, bookmarks, image servers.
- **DAG:** `0 → 1 → 2 → 3 → 5 → 6`, com a **Fase 4** (scenes/exchange)
  **paralelizável após a Fase 1**.
- **Arquitetura:** crate **novo e puro `ta-engine`** (só `ta-core` + `geo-types`;
  sem IO/DB/GUI).
- **Migrações:** `0005_phases`, `0006_aoi_cells`,
  `0007_grid_srid` (retificação da Decisão 4 em 2026-10-18),
  `0008_cell_validity` (gate de validade em `cells`), `0009_classes`,
  `0010_rules`, `0011_rule_class_restrict` (Fase 2 — FK
  `rules.class_id` com `ON DELETE RESTRICT`), `0012_queue`, `0013_drafts_measures`,
  `0014_scenes`.
- **Metas:** cobertura **70% global / 85%** em `ta-engine` e repos de `ta-db`;
  **zero `unwrap`/`expect`** em libs não-teste.

### Decisões registradas (não relitigar)

1. **(1 = a)** Paridade validada **apenas** por **documentação pública +
   observação comportamental**. Por política clean-room (`CONTRIBUTING.md`),
   **proibida** engenharia reversa do produto legado e **todo** o material
   proibido foi **removido do disco em 2026-10-18** por decisão do usuário;
   nenhum insumo derivado de engenharia reversa do produto legado entra no
   repositório.
2. **(2 = sim)** Nomes de tabela **próprios**, sem prefixo legado: `phases`,
   `aoi_layers`, `cell_layers`, `cells`, `blocked_cells`, `classes`,
   `class_groups`, `rules`, `rulesets`, `tasks`, `processes`, `cell_log`,
   `draft_layers`, `scenes`, `project_scenes`, `area_settings`, `app_meta`.
3. **(3)** Motor de domínio no crate **novo e puro `ta-engine`**.
4. **(4)** Decisão 4 (retificada pelo usuário em 2026-10-18): a grade é
   configurável em QUALQUER EPSG — resolução e origem nas unidades do SRS
   escolhido; armazenamento canônico das células em EPSG:4674; cálculo de área
   no SRS de `area_settings.srs_id` (default EPSG:5880). Não há escolha
   excludente entre graus e metros.
5. **(5)** Provisionamento na Fase 0 via `scripts/dev-up.sh` **autodetectando
   docker**, com **fallback conda** (`scripts/test-pg.sh`).

### Estado real (sem inflar progresso)

| Fase | Escopo | Estado |
|---|---|---|
| 0 | Usabilidade ponta a ponta (dev-up.sh, doctor, seed-demo, baseline, CI não-gating, docs) | **concluída** (2026-10-17/18) |
| 1 | Projeto / Fase / AOI / Células bloqueadas (`0005`, `0006`, `0007`, `0008`) | **implementada e revisada** (Themis, 2026-10-18: aprovada com ressalvas); achados aplicados, **aguardando commit** |
| 2 | Classes e regras/rulesets (`0009`, `0010`, `0011_rule_class_restrict`) | **implementada; revisão Themis aplicada** (2026-10-18), **aguardando commit** |
| 3 | Fila de processos + classificação básica (`0012_queue`, `0013_drafts_measures`) | **implementada; revisão Themis aplicada** (2026-10-18/19), **aguardando commit** — 306 testes; 13/13 migrações; grade de 118 células; retomada e claim concorrente provados |
| 4 | Scenes / exchange (`0014_scenes`) — paralelizável após a Fase 1 | **pendente** |
| 5 | Área, import/export e visualização | **pendente** |
| 6 | Paridade comportamental e fechamento | **pendente** |

**Fase 0 concluída; Fases 1–3 implementadas/revisadas, aguardando commit.**
Fases 4–6 não iniciadas. Nenhum ADR criado (não solicitado).

### Fechamento da revisão da Fase 1 (2026-10-18)

Themis aprovou com ressalvas; os achados de fechamento foram aplicados:

- `0008_cell_validity`: trigger `cells_geom_validity` (BEFORE INSERT OR UPDATE)
  rejeita `NOT ST_IsValid(geom)` com mensagem acionável — mesma política de
  `features`, sem reparo silencioso. Idempotente. As migrações **futuras**
  foram renumeradas: classes `0009`, rules `0010`, e (Fase 2 em diante)
  `0011_rule_class_restrict`, queue `0012`, drafts/measures `0013`, scenes `0014`.
- `bulk_insert` de células roda em **transação única** (falha em qualquer lote
  não deixa grade parcial — o seed decide idempotência por `count > 0`).
- `bulk_insert`/`bulk_insert_on` exigem `srid` **explícito** e o validam contra
  `cell_layers.srid` (`SridLayerMismatch`), em vez de reprojetar lixo calado.
- `seed-demo` imprime o id de `aoi_layers` (não o de `layers`) e reporta a
  contagem real de células após o insert.
- `grid info` rotula o SRS da origem e o da extensão (armazenamento 4674).
- `--srid -1` passa pelo clap nos dois formatos e cai na validação acionável
  (idem para origens negativas, ex.: `--origin-x -52.2`); `--help` sem markdown
  vazando (`**`).
- Teste métrico com tolerância **1e-6 m** e comparação **célula a célula**
  (4674→5880 vs. motor puro), além do envelope.
- **Números reais do fechamento:** **240 testes** (237 na revisão + 3 novos:
  gate `cells` inválida, atomicidade do `bulk_insert`, SRS divergente); grade
  4674/0.005 = **118 células**; grade 5880/500 m = **144 células**.
- `area_settings.srs_id` (nome real) corrigido em PLAN/STATUS; nota de backfill
  da `0007` (srid→4674, intencional) e do privilégio de FK
  (`GRANT REFERENCES ON spatial_ref_sys`) registradas.
- **Borda do recorte:** o motor usa `[x_min, x_max) × [y_min, y_max)`;
  `ST_Contains`/`ST_Covers` **não** são equivalentes na fronteira.
- **Adiado para o hardening da Fase 6:** dedup dos helpers de teste (n7), n8 e
  n13 — registrados, não corrigidos agora (YAGNI/escopo).

**Nada commitado** — o usuário faz commits manualmente. Arquivos em jogo neste
fechamento (todos na árvore de trabalho): `crates/ta-db/migrations/0008_cell_validity.sql`
(novo), `crates/ta-db/src/{repo_cell,error}.rs`, `crates/ta-core/src/cell.rs`,
`crates/ta-cli/src/{flow,seed,main}.rs`, testes de `ta-db`/`ta-cli`,
`crates/ta-gui/src/{db_worker,app}.rs`, `README.md`, `INSTALL.md`, PLAN/STATUS.

### Não commitado (registro do re-planejamento)

> Seção histórica, referente à criação do PLAN (re-planejamento domain-first),
> **não** ao fechamento da Fase 1 acima.

Novo: `.pantheon/deepwork/terraamazon-cleanroom-rust/PLAN-terraamazon-parity.md`.
Modificado: este `STATUS.md` (linha de ponte no cabeçalho + esta seção final).
Nenhum arquivo de código tocado.

### Fechamento da revisão da Fase 2 (2026-10-18)

Themis revisou a Fase 2 (classes/rulesets/regras na árvore de trabalho,
não commitada); os achados foram aplicados por Hermes — **nada commitado**.

- **F1 — cascata silenciosa:** migração **nova** `0011_rule_class_restrict`
  (idempotente) recria `rules.class_id → classes(id)` com
  **`ON DELETE RESTRICT`**. O repositório traduz a violação em
  `Error::ClassInUseByRules` (contagem + caminho de saída). `class delete`
  mostra a contagem e **bloqueia** sem `--force`; com `--force`, regras +
  classe caem numa **única transação** (`PgClassRepo::delete_with_rules`).
  Consequência de numeração registrada: `0012_queue` (Fase 3),
  `0013_drafts_measures` (Fase 3), `0014_scenes` (Fase 4).
- **F2 — corrida de versão:** `PgRuleSetRepo::create` substitui
  "MAX(version)+1 + retry único" por **`pg_advisory_xact_lock`**
  (`hashtext(project_id || ':' || name)`) envolvendo SELECT MAX + INSERT na
  mesma transação. Colisão residual vira `Error::VersionRace`
  ("corrida de versão …; tente novamente") e `duplicate_error` **não** a
  captura mais (não é "nome duplicado"). Prova: **6 processos CLI reais ×
  mesma linhagem × 5 rodadas → 6 criados, versões {1..6} sem buraco**
  (teste `cli_ruleset_seis_processos_paralelos_em_cinco_rodadas`, verde).
- **F3 — `not` + medida ausente (política decidida):** ausência **avaliada**
  propaga para a regra inteira, inclusive sob `not`/`or` — regra com
  comparação de medida ausente **nunca casa** (célula sem observação não é
  classificada por negação; ex.: `NOT NDVI >= 1` sem `ndvi` → "nenhuma
  classe"). A ausência é de medidas **efetivamente avaliadas**: curto-circuito
  de `and`/`or` pode ocultar medidas à direita (F7).
- **F4 — predicado inválido com rastro:** `Decision.skipped_invalid` +
  `skipped_invalid_names` (ordenado); `rule eval` imprime
  "regras ignoradas (predicado inválido): N [nomes]".
- **F6 — ciclos de `parent_id`:** escrita sobe a cadeia de mães (limite 100)
  e rejeita ciclo/auto-pai com `Error::ClassParentCycle` acionável.
- **F8 — escrita de `is_active`:** `class update --active <bool>` e
  `rule update --ruleset <id> --name <n> [--active b] [--priority p]
  [--when expr] [--class id]` com update **parcial real** no repo
  (`RulePatch` + `COALESCE($n, coluna)` — campo ausente preserva o valor).
  **Decisão documentada:** `classes.is_active` hoje é atributo de **exibição**
  — o motor não consulta classes; desativar classe não altera decisão
  (o motor NÃO foi mudado nesta rodada; `rules.is_active` sim é consultado).
- **F9/F10 — parser:** EBNF atualizada (aceita `.5`, `5.`, `+.5` — já
  aceitos pelo parser); literal não finito (`inf` por overflow) ou com mais
  de **17 dígitos significativos** (limite de distinção do `f64`) é rejeitado
  com `RuleError::NumberOutOfRange` ("número fora de intervalo").
- **F11 — código morto:** `RuleError::UnexpectedChar` removida (nunca
  construída); `Predicate::matches` removida (só testes a chamavam —
  `evaluate` é a API de avaliação; `matches_collecting` cobre o resto).
- **F12 — `resolve_project("")`:** nome vazio/só-espaços é rejeitado antes
  de tocar no banco (não auto-cria projeto sem nome).
- **F5 — REGISTRADO para a Fase 3 (não implementado):** integridade
  multi-projeto via triggers `BEFORE INSERT/UPDATE` comparando `project_id`
  em `rules.class_id ↔ rulesets.project_id`,
  `classes.class_group_id/parent_id` — requisito da fila de processos
  (`0012_queue`), ver PLAN-terraamazon-parity §Fase 3.

**Verificação real (2026-10-18):** `cargo fmt --all -- --check` ✓ ·
`cargo clippy --workspace --all-targets -- -D warnings` ✓ ·
`cargo test --workspace` ✓ (unitária) · `TA_TEST_PG=1 TA_PG_DSN=… cargo
test --workspace` ✓ (gated, PG16+PostGIS 3.4 real) · migração `0011` em
banco limpo 2× (11/11, depois "nada pendente") e em banco já em `0010`
(FK `confdeltype = 'r'` confirmada) · `class delete` bloqueia com contagem
("usada por 2 regra(s)") e `--force` apaga atômico · `rule eval` com
`NOT NDVI >= 1` sem `ndvi` → "nenhuma classe" · `rule eval` mostra
"regras ignoradas (predicado inválido): 1 [quebrada]" · ciclo A→B→A e
auto-pai rejeitados · `--when 'ndvi >= 999…9'` (32 dígitos) rejeitado ·
`resolve_project("")` rejeitado (zero projetos sem nome no banco) ·
`dev-up.sh` 2× idempotente.

Testes existentes atualizados com a nova semântica (motivo: F3 — regra com
medida ausente nunca casa; curto-circuito oculta medidas à direita): os
casos de `or`/`not` do motor foram reescritos para a política nova e o caso
que classificava `agua` por negação sem observação vira "nenhuma classe"
(`regra_not_sem_medida_nao_decide_classe`). Nenhum teste de produção
assumia a semântica antiga fora do motor.

### Fechamento da revisão da Fase 3 (2026-10-18/19)

Themis revisou a Fase 3 (fila de processos + classificação básica, na árvore
de trabalho, não commitada); os achados foram aplicados por Hermes —
**nada commitado**.

- **M1 — saída para o operador em delete com `draft_cells`** (FKs
  `ON DELETE RESTRICT` da `0013` ganharam caminho de saída; nada silencioso):
  - **`draft delete --project <id> --name <n> [--yes]`** (novo): mostra
    ANTES a contagem de células classificadas; sem `--yes` **bloqueia**
    (rascunho é derivado, mas a remoção é destrutiva — intenção explícita);
    com `--yes` apaga o rascunho e as decisões (CASCADE) reportando a
    contagem. Repo: `PgDraftLayerRepo::{get, count_cells, delete}`.
  - **`class delete`**: pré-checagem de `draft_cells` além das regras
    (repo `count_drafts_for_class` — contagem por rascunho COM nome). Sem
    `--force`: bloqueia com "classe `X` decide N célula(s) em M rascunho(s):
    …; apague o(s) rascunho(s) com `draft delete` ou repita com --force".
    Com `--force`: **uma transação** apaga draft_cells + regras + classe e
    **reporta as três contagens** (repo `delete_force` →
    `ClassForceDeleted`). O `delete` simples passou a **discriminar a FK**
    pelo nome da constraint: violação de `draft_cells_class_id_fkey` vira
    `Error::ClassInUseByDraftCells` (antes seria relatado como "usada por
    regra(s)" mesmo sem regra).
  - **`ruleset delete --project <id> --name <n> [--version v] [--force]`**
    (novo): mesma pré-checagem (`count_drafts_for_ruleset`); sem `--force`
    com decisões: bloqueia; com `--force`: draft_cells + regras + ruleset
    numa transação com as três contagens (`RuleSetForceDeleted`); sem
    decisões: apaga com contagem de regras em cascata. O `delete` do
    ruleset também traduz a FK (`Error::RuleSetInUseByDraftCells`).
- **m2 — `task cancel` de task concluída**: recusado com
  "task `X` já concluída; nada a cancelar" quando
  pendentes+running+error = 0 — `done` (histórico de sucesso) não vira
  `cancelled`; task `failed` continua cancelável (cancelling impede retry).
- **m3 — `DraftCell` morto**: tipo removido de `ta-core` (nunca usado em
  assinatura — as linhas de rascunho gravam por tuplas em
  `draft_upsert_on`; YAGNI).
- **m5 — rascunho de outra grade**: `task create` (rascunho existente) e
  `task run` (pré-voo, inclui rascunho APAGADO como erro próprio) validam
  `task.cell_layer == draft_layer.cell_layer` — erro acionável
  ("o rascunho `X` pertence à grade `Y`, mas a task usa a grade `Z`").
- **Registros (não implementar agora)**: Fase 4 — `cell_measures` com
  proveniência por cena (`scene_id` NULL FK→scenes; last-write-wins por
  `(cell_layer, cell_id, measure)`; PK inalterada) e Fase 5 — extração do
  executor para crate próprio (`ta-flow`) antes da GUI; risco transversal
  novo no PLAN §11: **toda FK RESTRICT nova vem com pré-checagem +
  mensagem acionável + caminho de saída** (padrão M1).

**Números reais do fechamento:** **306 testes** (300 na revisão + 6 novos:
5 CLI gated — `draft delete` sem/com `--yes`, `class delete` bloqueando com
contagens/nomes e `--force` com 3 contagens, `ruleset delete` idem,
`task cancel` de done recusado, rascunho de outra grade recusado no create
e no run — e 1 repo gated: recusas tipadas + discriminação de FK +
`delete_force` com as três contagens). Migrações **13/13**. `DraftCell`
removido sem quebrar compilação (nenhum uso real).

**Verificação real (2026-10-19):** `cargo fmt --all -- --check` ✓ ·
`cargo clippy --workspace --all-targets -- -D warnings` ✓ ·
`cargo test --workspace` ✓ (unitária) · `TA_TEST_PG=1 TA_PG_DSN=… cargo
test --workspace` ✓ **306/306** (PG16+PostGIS 3.4 real) · ponta a ponta
real pelos testes CLI: classe→regra→medidas→`task run`→`draft_cells`, e os
seis fluxos de delete/cancel/grade listados acima provados contra o binário
real · `dev-up.sh` 2× idempotente.

**Nada commitado** — o usuário faz commits manualmente.

## Fase 4 do fluxo — SCENES E EXCHANGE (@hermes, 2026-10-19)

**Escopo entregue** (PLAN §6, Fase 4; migração `0014_scenes`; desenho
aprovado no briefing — divergências listadas abaixo):

- **Migração `0014_scenes`** (idempotente): `scenes` (global,
  `UNIQUE(name)`, aquisição/footprint opcionais, `footprint
  geometry(MultiPolygon,4674)`, `metadata jsonb`), `project_scenes`
  (N:N com CASCADE duplo, `linked_on`), `cell_measures.scene_id` NULL
  FK→`scenes` `ON DELETE SET NULL` + índice (proveniência; a PK
  `(cell_layer_id, cell_id, measure)` e a política **last-write-wins**
  ficam inalteradas, documentadas na migração), e
  `projects.description` (hospeda a descrição da seção `project` do
  exchange — ver divergência d1).
- **Exchange `curupira-monitoring-exchange v1`** — modelo serde e dono do
  formato em `ta-gis::exchange` (SEM dependência de banco, sem GDAL:
  JSON puro + `wkt` 0.14 para geometria, já no lock via geozero).
  Portabilidade por NOMES (projeto, fase, camada, `cell_id`, classe,
  ruleset nome+versão, cena). `deny_unknown_fields` (typo não perde dado
  silenciosamente), validação de `format`/`version` com erros tipados,
  WKT com contexto na mensagem (`cell_layers[0].cells[2].wkt`).
- **`exchange export --project <id|nome> --out <arquivo>`**: resolve
  projeto EXISTENTE (sem auto-criar), coleta TODAS as seções por UUID e
  escreve JSON pretty com contagens por seção na saída; seções vazias
  são válidas (projeto recém-criado exporta).
- **`exchange import --in <arquivo> --into <novo-nome>`**: RECUSA projeto
  existente (sem merge nesta fase); recria as seções na ordem de
  DEPENDÊNCIA — projeto(+settings) → **cenas(+vínculos)** → fases →
  AOI(camada+features+vínculo) → grades(células bulk+bloqueios) → grupos
  → classes (**ordem topológica por `parent_name`; ciclo é erro antes do
  banco**) → rulesets versão explícita (+regras com `parse_predicate`
  ANTES de gravar, erro aciona a regra) → rascunhos(+decisões) →
  medidas(upsert). Uma transação por seção; célula de exchange entra em
  WKT 4674 direto (sem `ST_Transform` — novo `import_cells_4674_on`).
- **`scene add/list/link/unlink/remove`**: `remove` é destrutivo com
  `--yes` (sem ele mostra projetos vinculados + medidas que perdem a
  proveniência); cena global reaproveitada pelo import quando o nome já
  existe (política documentada; `SceneDuplicate` tipado com caminho de
  saída).
- **`measure set --scene <nome>`**: proveniência resolvida por nome,
  erro acionável ("não existe; crie com `scene add`"). Regrar sem
  `--scene` SUBSTITUI a proveniência anterior (last-write-wins).
- **ta-db**: `repo_scene` novo; variantes `_on(exec/conexão)` para o
  import transacional (`phase/aoi/class/class_group/draft/rule/
  ruleset-with-version/activate/block/scene create+link/feature-insert`);
  `PgDraftLayerRepo::list_cells` (leitura de decisões para o export);
  `PgProjectRepo::flow_settings`/`create_with_flow_settings`;
  `upsert_measures_on`/`list_measures` com `scene_id`.

**Divergências do desenho aprovado (todas mínimas e documentadas):**
- **d1 — `projects.description`**: o schema não tinha descrição de
  projeto; a coluna foi adicionada NA `0014` para hospedar o campo
  `description` da seção `project` do formato aprovado (nenhum comando a
  escreve; entra/sai pelo exchange — documentado na migração).
- **d2 — `aoi_layer_name` opcional em `cell_layers`**: campo ADITIVO ao
  formato; `cell_layers.aoi_layer_id` é NOT NULL no banco e a grade
  precisa saber qual AOI recriar. Sem o campo: 1 AOI no projeto resolve
  sozinha; 0 ou N AOIs → erro acionável pedindo o campo.
- **d3 — cenas ANTES de medidas no import**: a lista do desenho terminava
  em "…measures→scenes+links", mas a medida referencia a cena
  (`scene_name`); a ordem de dependência exige cenas primeiro (a seção
  `scenes` do DOCUMENTO continua no fim, como aprovado).
- **d4 — import REAPROVEITA cena existente pelo nome** (cena é global,
  `UNIQUE(name)`): criar duplicaria erro no round-trip export→import no
  MESMO banco. Vínculo com o novo projeto é sempre gravado.
- **d5 — múltiplos rulesets ativos no documento**: o índice parcial único
  (máx. 1 ativo por projeto) é lei; o import ativa o PRIMEIRO ativo do
  documento e reporta. (`rule eval` idem: ativo é o do projeto alvo.)
- **d6 — export de cena referenciada só por medida**: entra na seção
  `scenes` também (senão o import não resolveria `scene_name`); em
  prática o projeto a tem vinculada.

**Fora do escopo (intencional):** merge de import (recusado, documentado),
GDAL no exchange (JSON puro), GUI, área (Fase 5), matriz de paridade
(Fase 6). `ta-gui` não foi tocada.

**Testes (+18; 324 no total):**
- Unitários sem banco (9 em `ta-gis::exchange`): round-trip serde do doc
  de exemplo, seções vazias válidas, `format`/`version` errados recusados,
  seção desconhecida recusada (aponta linha), WKT inválido com contexto,
  footprint exige (Multi)Polygon, ISO-8601 (RFC-3339 e data simples),
  WKT do motor reparseia; 2 em `ta-core::scene` (defaults serde).
- Gated (4 em `ta-db/tests/pg_fase4_scenes.rs`): `UNIQUE(name)` tipado,
  footprint EWKB↔PostGIS como MultiPolygon 4674, vínculo idempotente com
  CASCADE e `projects_using`/`measures_using`, medida sobrevive ao delete
  da cena com SET NULL + last-write-wins da proveniência.
- Gated E2E pelo binário (3 em `ta-cli/tests/flow_fase4_cli.rs`):
  **round-trip completo** (projeto com fase, AOI, grade 2 células, 1
  bloqueio, grupo+2 classes com mãe, ruleset v1 + 2 regras, `task run`
  classificando, medidas com cena) → export → import → **contagens iguais
  em 10 tabelas/views**, **igualdade geométrica `a.geom = b.geom` por
  (nome da grade, cell_id)** (0 diferenças), cena reaproveitada com
  aquisição/footprint idênticos (`ST_Equals`), decisões iguais POR NOME de
  classe, medidas iguais, **mesma decisão `rule eval`** nos dois projetos
  para a amostra `ndvi=0.72`; e as **6 recusas**: projeto existente, JSON
  corrompido, versão 99, classe inexistente (aponta a regra), predicado
  inválido (aponta a regra), projeto fantasma no export; ciclo de vida
  `scene add/list/link/unlink/remove` com recusa sem `--yes`;
  `measure set --scene` (erro acionável + saída com a cena).

**Provas de migração (2026-10-19, conda PG16+PostGIS 3.4):** banco LIMPO →
`migrate` aplica **14/14** e a 2ª execução reporta "nada pendente";
banco JÁ EM 0013 (Migrator sem a 0014) → `migrate` incremental aplica
**só a 0014** (14/14; `scenes`, `project_scenes` e
`cell_measures.scene_id` confirmados no catálogo). `dev-up.sh` 2×
idempotente (doctor exit 0, seed "já existia", 118 células). Round-trip
adicional com o projeto `demo` do seed: 118 células exportadas/importadas
com **0** diferenças em `a.geom = b.geom`.

**Verificação real:** `cargo fmt --all -- --check` ✓ ·
`cargo clippy --workspace --all-targets -- -D warnings` ✓ ·
`cargo test --workspace` ✓ · `TA_TEST_PG=1 TA_PG_DSN=… cargo test
--workspace` ✓ **28/28 binários, 324 testes** (gated executados de fato —
zero "skipped").

**Nada commitado** — o usuário faz commits manualmente.

## Fechamento da Fase 4 — achados da revisão (Themis) aplicados (@hermes, 2026-10-19)

A revisão da Fase 4 apontou 1 MAJOR, 2 MINOR e 2 NIT; todos aplicados SEM
iniciar a Fase 5 (área/GUI/ta-flow) e SEM commit:

- **MAJOR-1 — recuperabilidade de import falho:**
  1. **Import ATÔMICO** — `exchange import` agora roda projeto + TODAS as
     seções numa ÚNICA transação (`import_sections` sobre
     `&mut PgConnection`; novas variantes executor: `PgProjectRepo::create_on`,
     `PgSceneRepo::get_by_name_on`, `PgCellRepo::exists_on` — o que a seção
     anterior gravou só é visível dentro da transação). Falha em qualquer
     seção NÃO deixa NADA no banco (nem projeto parcial) e o mesmo `--into`
     pode ser repetido imediatamente; a recusa de nome existente continua
     (duplicata agora só quando o import ANTERIOR concluiu). A mensagem de
     falha cita o caminho de saída exato:
     `project delete --project <nome> --yes` (defesa em profundidade — com o
     rollback nada deve restar).
  2. **`project delete [--yes]`** — deleção ORDENADA em transação única
     (`PgProjectRepo::delete_ordered` + `ProjectDeleteReport`), respeitando as
     FK `ON DELETE RESTRICT` SEM desativar triggers: draft_cells → rules →
     rulesets → cell_log → processes → tasks → cell_measures → blocked_cells →
     cells → cell_layers → draft_layers → **aoi_layers → features → layers**
     (aoi_layers ANTES de layers: `layer_id` é CASCADE e o relatório precisa
     atribuir cada linha à tabela certa) → project_scenes → classes →
     class_groups → phases → project. Relatório com contagens por tabela
     (18 rótulos + `total=`). Sem `--yes`: recusa mostrando o resumo do que
     seria apagado (`delete_preview`, mesmas contagens). Cenas são globais:
     sai só o vínculo, `scenes` permanece. A deleção NÃO auto-cria projeto.
  3. Prova real (projeto `demo` 118 células, import com falha injetada no
     MEIO do doc — WKT inválido em `cell_layers[0].cells[7]`): falha →
     `select count(*) from projects where name='demo_atomo2'` = **0** →
     retry do MESMO `--into` **funciona** → `project delete --yes` reporta
     `total=122` (cells=118, cell_layers=1, aoi_layers=1, features=1,
     layers=1) e o banco fica zerado por id capturado.
- **MINOR-1 — cena reaproveitada com divergência:** no import, cena existente
  (nome é global) tem footprint (WKT normalizado pelo mesmo serializador) e
  metadata comparados; divergência emite `aviso: cena \`X\` já existia com
  footprint/metadata diferentes — reaproveitada sem alteração` no relatório.
  Não falha, não sobrescreve (teste gated: footprint trocado no doc → aviso
  presente + `ST_Equals` do conteúdo original + vínculo do projeto novo).
- **MINOR-2 — float_roundtrip:** `serde_json = { version = "1", features =
  ["float_roundtrip"] }` no workspace (afeta exchange inteiro). Teste
  unitário: `9643762.620782405` no campo `value` de medida → export 2× com
  bytes idênticos e f64 preservado no reparse.
- **NIT-1:** mensagem de pai de classe agora diz "parent_name com ciclo ou
  mãe inexistente entre …" (antes: "forma ciclo" — impreciso, o limite de
  passos também dispara para mãe inexistente).
- **NIT-2:** `scene add --footprint` distingue WKT inválido ("não é WKT
  reconhecível e o arquivo não existe") de WKT válido não-poligonal
  (`FootprintNotPolygonal` repaginado: "esperado POLYGON/MULTIPOLYGON,
  recebido POINT") — POINT não caía em "não reconhecível". 3 testes
  unitários em `flow_scene`.
- **Risco permanente reafirmado (PLAN §10 risco 7):** FK RESTRICT ⇒
  pré-checagem com contagens + caminho de saída explícito — agora também no
  apagado de PROJETO inteiro (`project delete` é o caminho de saída do import
  falho e de qualquer projeto rico com rules/draft_cells).

**Testes:** +7 (331 no total, 28 binários): 1 unitário float_roundtrip
(`ta-gis::exchange`), 3 unitários de footprint (`ta-cli::flow_scene`), 3
gated E2E (`flow_fase4_cli.rs`): import falho não deixa projeto parcial e
retry funciona (inclui asserção da mensagem com o comando exato de saída),
cena divergente emite aviso e é reaproveitada sem sobrescrita,
`project delete` sem/--yes com 18 tabelas zeradas POR ID capturado antes da
deleção (join com projects zeraria trivialmente) e cena global sobrevivente.

**Verificação real:** `cargo fmt --all -- --check` ✓ ·
`cargo clippy --workspace --all-targets -- -D warnings` ✓ ·
`cargo test --workspace` ✓ · `TA_TEST_PG=1 TA_PG_DSN=… cargo test
--workspace` ✓ **28/28 binários, 331 testes** (gated executados — zero
"skipped") · `dev-up.sh` 2× idempotente (doctor exit 0).

**Nada commitado** — o usuário faz commits manualmente.
