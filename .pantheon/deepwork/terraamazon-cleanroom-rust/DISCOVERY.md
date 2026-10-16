# DISCOVERY — terraamazon-cleanroom-rust

> Fase DISCOVERY do workflow deepwork — preenchido por @apollo (scout read-only), **2026-10-16**.
> Método: leitura integral dos 5 crates + comandos read-only capturados no host (PTY). Nada foi instalado e nada foi modificado fora deste arquivo.
> Este documento **corrige dois registros** da verificação da Fase 0 em `STATUS.md» (libgdal-dev presente; rustc só em login shell) — ver Environment Survey.

## Codebase Map

### Workspace (raiz)
- `Cargo.toml»: resolver 2; members `ta-core/ta-gis/ta-db/ta-gui/ta-cli»; edition 2024; rust-version 1.85. `rust-toolchain.toml»: stable + rustfmt + clippy. CI (`.github/workflows/ci.yml»): fmt → clippy `-D warnings» → test → cargo-deny; **sem bloco `services:» (nenhum PG no CI hoje)**.
- Deps de workspace: geo-types 0.7 (serde), serde 1, serde_json 1, thiserror 2, time 0.3; IO opcional: gdal 0.19, geojson 1.0, shapefile 0.9, csv 1.4, geozero 0.15; persistência: **sqlx 0.9 (`default-features = false», features `runtime-tokio»+`postgres»)**, rusqlite 0.40 (bundled, opcional), async-trait 0.1; GUI/CLI: eframe 0.37, clap 4.6. `Cargo.lock» commitado.
- Ainda não existem: `migrations/», `tests/», `scripts/», `testdata/» — todos nascem na Fase 1.

### ta-core — domínio puro (sem IO/banco)
- `project.rs»: `DEFAULT_SRID = 4674» (SIRGAS 2000); `Project { id, name, srid (serde default 4674), created_at (RFC3339) }»; 2 testes.
- `layer.rs»: `GeomType {Point..GeometryCollection, Raster}»; `Layer { id, project_id, name, geom_type, srid (default 4674), source_uri }».
- `feature.rs»: `FeatureId = u64»; `Feature { geom: Geometry<f64>, attrs: serde_json::Value }».
- `style.rs»: `Style { color "#RRGGBB[AA]", stroke_width, opacity, visible }».
- `error.rs»: `NotImplemented», `UnsupportedFormat», `SridMismatch {expected, got}», `Json», `Io».
- **`geo.rs» = ponto de extensão principal da Fase 1**: `to_wkb / from_wkb / from_ewkb / to_ewkb» todos retornam `NotImplemented» (stub Fase 0). Doc fixa: implementar com `geozero»; EWKB carrega SRID embutido; é o PONTO ÚNICO de conversão (ta-db e ta-gis passam por aqui).

### ta-gis — IO geoespacial (features opcionais: `gdal», `geojson», `shapefile», `csv», `geozero»; default enxuto)
- `reader.rs»: `VectorFormat {Shapefile,GeoJson,GeoPackage,CsvXy}», `RasterFormat {GeoTiff,Cog}», `ReadOptions {x_col,y_col,srid}», `RasterInfo»; stubs `detect_vector_format / read_vector / read_raster» (Fase 2).
- `writer.rs»: `WriteFormat» (5 formatos), `WriteOptions {target_srid=4674}»; stub `write_vector» (Fase 2).
- `normalizer.rs»: `TARGET_SRID=4674», `NormalizationReport {input_srid,reprojected,invalid_geometries,repaired_geometries}»; stub `normalize» (Fase 2; política ST_IsValid/ST_MakeValid).
- `derived.rs»: `DerivedProduct {Cog,Mvt,FlatGeobuf}»; stub `build» (Fase 2; MVT via ST_AsMVT no PostGIS).
- `error.rs»: `UnsupportedFormat(String)» tipado + `NotImplemented» + `Core».

### ta-db — persistência (deps: ta-core, async-trait, geozero, thiserror, sqlx; feature opt-in `offline-gpkg» = rusqlite)
- `pool.rs»: **código real** — `DbConfig {url: postgres://ta:ta@localhost:5432/ta, max_connections: 8}»; `connect()» via `PgPoolOptions». Teste unitário do default.
- `repo.rs»: **contratos async-trait sem implementação (Fase 1 implementa)**: `ProjectRepo {create,get,list,delete}», `LayerRepo {create,list_by_project,delete}», `FeatureRepo {bulk_insert(layer_id, Vec<Feature>) -> u64}». Regras documentadas: bind-only (gate themis), bulk via **COPY BINARY** (meta 10k), erros em `crate::Error».
- `spatial.rs»: `TARGET_SRID=4674» + `FEATURES_DDL_STUB» (DDL comentado = contrato-alvo: `features(geom geometry(Geometry,4674) NOT NULL, attrs jsonb, GIST, CHECK ST_SRID=4674, FK layers ON DELETE CASCADE)»). **Fase 1 substitui por migração sqlx real (`sqlx::migrate!»)**.
- `error.rs»: `NotImplemented», `Core(ta_core::Error)», `Sql(sqlx::Error)».

### ta-gui — desktop egui/eframe
- `map_view.rs» (completo + 2 testes): trait `MapView {set_layers, set_crs, render, pick, zoom_to}»; tipos `LayerHandle», `Bbox», `RenderCtx {egui, viewport_px}»; única impl `MapViewEgui» (render stub).
- `main.rs»: bin eframe com a API nova `eframe::App::ui(...)» (ajuste já feito na F0). Sem dependência de ta-db ainda.

### ta-cli — CLI (clap derive; todos os subcomandos saem código 2)
- `import-vector {--input,--srid?}», `import-raster {--input}» (Fase 2), `list-layers {--project?}» (Fase 1), `migrate {--source}» (Fase 1; doc: dump SQL-text + ogr2ogr → EPSG:4674; **nunca PG10**).

### Pontos de extensão que a Fase 1 preenche
1. `ta_core::geo» — WKB/EWKB via geozero (ponto único de conversão).
2. `ta_db::repo» — implementações Pg dos 3 traits (bind-only; COPY BINARY no bulk_insert).
3. `ta_db::spatial» + novo `migrations/0001_*.sql» + `sqlx::migrate!» (GIST, CHECK SRID, jsonb).
4. `ta-cli»: `migrate» (SQL-text + ogr2ogr) e `list-layers».
5. Novos diretórios: `migrations/», `scripts/» (harness PG), `tests/» de integração gated.

## Environment Survey

Host: Ubuntu 24.04.3 LTS, kernel 6.8.0-142, x86_64, usuário `jovyan», 220 GiB RAM, 1.8 TiB livres em `/». Comandos executados (leitura): `uname -a», `command -v docker/podman/nerdctl/psql/…», `dpkg -l | grep -iE "postgres|postgis|gdal"», `apt-cache policy …», `sudo -n true», `gdalinfo/ogr2ogr --version», `rustc/cargo --version» (login shell), `mamba search -c conda-forge …» (só busca de catálogo), `ogrinfo --formats».

| Ferramenta | Versão encontrada | Disponível? | Viabilidade de instalar |
|---|---|---|---|
| docker / docker compose | — | ❌ `docker: command not found» (confirma registro F0) | ❌ sudo bloqueado |
| podman / nerdctl | — | ❌ não encontrados | ❌ sudo bloqueado |
| psql (cliente) | — | ❌ ausente | ⚠️ vem junto no env conda (rota 4 abaixo) |
| pg_config (sistema) | **PostgreSQL 16.15** (`/usr/bin/pg_config»), `libpq-fe.h» presente (libpq-dev 16.15) | ✅ | n/a (já instalado) |
| pg_config (conda base) | 18.1 (`/opt/conda/bin») | ✅ (sombreado por PATH) | n/a |
| Servidor PostgreSQL | — | ❌ sem `/usr/lib/postgresql», sem processo, porta 5432 fechada, `initdb» ausente | ✅ **conda-forge: `postgresql 16.15 h23f1c25_0 linux-64» (busca confirmou)** |
| PostGIS | dpkg `postgis 3.4.2» = só CLIs (shp2pgsql, raster2pgsql, postgis_restore, pgsql2shp); **sem server/extensão** | ⚠️ parcial | ✅ **conda-forge: `postgis 3.4.3 h3473a85_2» (+3.4.2/3.4.1/3.4.0)** |
| apt postgresql-16 | candidato vazio (`apt-cache policy» vazio; lists só CRAN) | ❌ | ❌ **`sudo: The "no new privileges" flag is set» → rota apt MORTA** |
| GDAL CLI (gdalinfo/gdal-config/ogr2ogr/ogrinfo) | **3.8.4** (2024/02/08) | ✅ todos | n/a |
| libgdal-dev | **3.8.4+dfsg-3ubuntu3** (dpkg; `libgdal.so.34» no ldconfig) | ✅ **PRESENTE — corrige STATUS.md (F0 disse ausente)** | n/a |
| rustc / cargo | **1.99.1 stable** + rustup + clippy/rustfmt + cargo-deny 0.20.2 (`~/.cargo/bin») | ✅ **somente em login shell (`bash -lc»)** — PATH não-login não resolve | n/a |
| Python | 3.11.15 (venv ativo) + pip 26.1.1; system `python3-gdal 3.8.4»; conda env `geospatial» | ✅ | n/a |
| conda / mamba | 25.9.1 / 2.5.0; rede p/ conda-forge OK | ✅ | envs em `$HOME/.conda/envs» (escrita ✅; `/opt/conda/envs» não) |

Notas:
- Artefatos F0 confirmados: `target/debug/{ta-cli, libta_core.rlib, libta_db.rlib}» — o build local da Fase 0 foi real.
- Aviso de loader ao rodar GDAL CLI: `libcurl.so.4» do env conda `geo_pipeline» é carregado (LD_LIBRARY_PATH). Inofensivo p/ as checagens, mas evite shell com env conda ativado p/ builds Rust com GDAL.
- sqlx é puro Rust (não usa libpq); o libpq 16.15 do sistema só importa p/ clientes externos (ex.: driver PG do GDAL, presente).

## Test Harness Decision (Fase 1)

Opções avaliadas contra o host REAL:
1. **testcontainers(-rs)** — ❌ exige runtime de container; docker/podman/nerdctl ausentes, sudo bloqueado. Morte neste host.
2. **Serviço PG do sistema** — ❌ nenhum servidor instalado, sem `initdb», sem sudo. Morta.
3. **apt install PG16** — ❌ sudo bloqueado (`no new privileges») + apt lists vazias. Morta.
4. **PG16+PostGIS 3.4 efêmero via conda no $HOME + `initdb» em temp dir** — ✅ **RECOMENDADO (existe no host)**. Evidência: `mamba search -c conda-forge 'postgresql>=16,<17'» → `postgresql 16.15»; `mamba search -c conda-forge 'postgis>=3.4,<3.5'» → `postgis 3.4.3». `conda info»: envs_dirs prioriza `/home/jovyan/.conda/envs» (escrita confirmada). Sem root.
5. **CI-only** — ⚠️ complementar: CI hoje não tem `services:»; adicionar `postgis/postgis:16-3.4» como service do GH Actions cobre a lane remota, mas não substitui verificação local.

**Decisão: opção 4.** Setup único (exige aprovação do usuário — é uma instalação de pacotes):
`mamba create -y -p "$HOME/.conda/envs/ta-pg16" -c conda-forge postgresql=16.15 postgis=3.4.3»

Por execução (script `scripts/test-pg.sh», a criar na Fase 1):
1. `initdb -D "$TMPDIR/ta-pgdata-$$" -A trust -U ta»
2. `pg_ctl -D … -o "-p 55432 -k $TMPDIR" -l … start» (porta alta fixa)
3. `createdb -p 55432 ta» + `CREATE EXTENSION postgis»
4. exporta `TA_PG_DSN=postgres://ta:ta@127.0.0.1:55432/ta»
5. roda a suíte de integração gated (feature `pg-integration» ou testes `#[ignore]» condicionados a `TA_PG_DSN»), depois `pg_ctl stop -m fast» + limpeza.

Alinhamento à economia de testes: unit tests nunca tocam PG; suíte PG é opt-in por env var; CI ganha o service container na Fase 1.
Pins: compose `16-3.4», conda `16.15 + 3.4.3» — mesma major PG, mesma minor PostGIS; documentar no script.

## Bench Dataset Spec (Fase 3)

Gerador versionado `scripts/gen_bench_fixtures.py» (Python 3.11 + `osgeo.gdal» do sistema 3.8.4 — já instalado; zero deps extras), `--seed» default **42**, determinístico; saída em `testdata/bench/» (gerado, NÃO commitado; commitam-se script + `manifest.json» com sha256):

1. **`bench_polys.shp»** (+.shx/.dbf/.prj) — **N = 10.000 polígonos irregulares** (anel de 8–20 vértices com perturbação pseudo-random seeded, sem auto-interseção), CRS **EPSG:4674**, bbox lon [-73,-50] / lat [-10,2]; atributos: `id» int32, `nome» str(24) `poligonal_%05d», `area_ha» float calculado, `classe» str(12) ciclando em `floresta|pastagem|agricultura|agua|desmatamento». Justificativa: 10k = meta de bulk insert de `FeatureRepo» (COPY BINARY).
2. **`bench_points.csv»** — **50.000 linhas** CSV-XY (`lon,lat» EPSG:4674 + 5 attrs determinísticos) — caminho CSV-XY + volume.
3. **`bench_raster.tif»** — GeoTIFF **1024×1024, 4 bandas UInt16** (gradientes + ruído seeded), tiled 256×256, geotransform EPSG:4674, nodata=0, compressão LZW.
4. **`bench_raster_stress.tif»** (flag `--stress») — **8192×8192, 1 banda UInt16** — insumo do gatilho objetivo da Fase 3b (egui vs Tauri+MapLibre com raster grande).
5. **`manifest.json»** — `{arquivo, sha256, bytes, rows|dims, params{seed,N}}»; teste leve regenera e compara sha256 (determinismo comprovado).

Critérios: geração < ~1 min; nenhum dado real/legado (barreira clean-room).

## Migrate Tooling

Decisão do Gate 1 (SQL-text + ogr2ogr, sem pg_dump custom) **confirmada e suportada pelo host**:
- `ogr2ogr»/`ogrinfo» **3.8.4** ✅ (pg_dump/psql nem existem no host — a decisão evita dependência morta).
- **Driver PostgreSQL/PostGIS presente** — evidência `ogrinfo --formats»: `PostgreSQL -vector- (rw+): PostgreSQL/PostGIS» e `PGDUMP -vector- (w+v): PostgreSQL SQL dump». Ou seja: ogr2ogr lê o banco legado E emite dump SQL, além do reprojeto `-t_srs EPSG:4674».
- Bônus: `shp2pgsql»/`raster2pgsql»/`postgis_restore» 3.4.2 (dpkg postgis) p/ diagnósticos.
- Fluxo sugerido p/ `ta-cli migrate»: 1) ingest de dump SQL-text (execução raw em schema staging); 2) fontes arquivo via `ogr2ogr -t_srs EPSG:4674 -f PostgreSQL PG:"…"» (ou leitura ta-gis na Fase 2); 3) validação pelo CHECK `ST_SRID=4674» + round-trip de teste; 4) recusar origem < PG16 (checar `SELECT version()»; requisito "nunca PG10").

## Risks & Recommendations

| # | Risco | Recomendação |
|---|---|---|
| R1 | Sem container runtime e **sudo bloqueado** (`no new privileges») — apt/docker/podman todos mortos neste host | Harness conda efêmero (decisão acima). Único "install" do workflow inteiro: `mamba create» — pedir aprovação explícita do usuário. |
| R2 | PG indisponível se o download conda falhar na prática ([UNKNOWN] até a 1ª tentativa — busca confirmou canal/builds, instalação não executada) | Fallback em 2 níveis: (a) testes PG `#[ignore]» + script documentado; (b) **usar API runtime `sqlx::query()» (não macros compile-time) enquanto não houver PG dev** — macros `query!» exigem DB vivo (ou cache `.sqlx» via `cargo sqlx prepare») e quebrariam `cargo test --workspace»/CI sem serviço. Migrar p/ macros depois, se quiser, com o service do CI. |
| R3 | STATUS.md registra `libgdal-dev ausente» — **incorreto hoje** (3.8.4 completo presente) | Corrigir registro; **rodar `cargo check -p ta-gis --features gdal» já na Fase 1** (barato; mata cedo o risco da Fase 2). Manter prereq no README p/ outros hosts. |
| R4 | `rustc/cargo» só no PATH de login shell | Todo script/agente deve usar `bash -lc» ou `export PATH="$HOME/.cargo/bin:$PATH"». |
| R5 | Interferência conda↔sistema no GDAL CLI (warning de libcurl observado) | Rodar GDAL CLI em shell sem env conda ativado; garantir que builds Rust não herdem `LD_LIBRARY_PATH» de envs conda (binding usa `libgdal.so» do ldconfig). |
| R6 | `migrations/» não existe; DDL é stub comentado em `spatial.rs» | Criar `migrations/0001_init.sql» (projects/layers/features; geometry(Geometry,4674) NOT NULL; GIST; CHECK ST_SRID=4674; attrs jsonb; FK CASCADE) + `sqlx::migrate!»; remover `FEATURES_DDL_STUB» quando a migração existir (dead code). |
| R7 | Drift de versão entre compose (16-3.4), conda (16.15/3.4.3) e CI futuro | Pin explícito nos 3 lugares; suíte de integração deve passar nos 3 (PG16 major comum, PostGIS 3.4 minor comum). |
| R8 | CI sem serviço PG (só fmt/clippy/test/deny) | Adicionar `services: postgis/postgis:16-3.4» quando a Fase 1 landar; manter suíte PG gated p/ não inflar o loop de dev. |

[UNKNOWN] declarados: sucesso real do `mamba create» (nunca executado — fora da missão); `initdb» conda-forge + PostGIS do mesmo env (padrão maduro, não testado aqui); CI remoto (repo com `git init» mas **0 commits**, sem remote).
