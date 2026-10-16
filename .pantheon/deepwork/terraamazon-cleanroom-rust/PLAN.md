# Plan: terraamazon-cleanroom-rust — Clean-Room tipo TerraAmazon 7.3.2 em Rust
## Goal
Criar do zero (clean-room estrito) um GIS desktop moderno em Rust que replica o escopo funcional da linha TerraAmazon 7.3.2 (projeto/banco, importação vetorial/raster, visualização, edição básica, exportação) sobre stack atual: egui+eframe, sqlx+PG16/PostGIS 3.4, GDAL 3.8.
## Scope
In: workspace Rust (ta-core/ta-gis/ta-db/ta-gui/ta-cli), licença dual MIT OR Apache-2.0 + barreira clean-room, import MVP SHP/GeoJSON/GPKG/GeoTIFF + CSV-XY, visualização 2D, edição básica, derivados COG/MVT(ST_AsMVT)/FlatGeobuf, PG16+PostGIS 3.4 primário (SRID 4674, GIST, COPY BINARY, sqlx-migrate), GPKG offline opt-in, ta-cli migrate (SQL-text + ogr2ogr + ST_Transform→4674), GUI egui sobre trait MapView, bench de decisão p/ Fase 3b (Tauri+MapLibre) condicional.
Out: qualquer uso do binário/zip legado; WFS-T/KML/NetCDF/DWG; iced; diesel/postgis_diesel; GDAL estático.
## Phases
- Fase 0: Scaffolding, licenciamento e barreira clean-room (esta fase).
- Fase 1: Persistência PostGIS + migrações + ta-cli migrate — schema projects/layers/features com geometry(Geometry,4674) NOT NULL + GIST + CHECK ST_SRID=4674; repos sqlx async SQL direto; bulk COPY BINARY; sqlx-migrate; fallback GPKG feature; migrate lê dumps SQL-text + arquivos (ogr2ogr -t_srs EPSG:4674), nunca roda PG10. Testes: integração PG16 real, round-trip 4674, rejeição SRID≠4674, bulk 10k.
- Fase 2: IO geoespacial MVP + derivados — leitura/escrita SHP/GeoJSON/GPKG/GeoTIFF + CSV-XY; COG+overviews; MVT via ST_AsMVT; FlatGeobuf write; WMS cache opcional flag; normalização p/ 4674 + política ST_IsValid/ST_MakeValid; UnsupportedFormat tipado p/ KML/NetCDF/DWG. Testes: golden round-trip por formato.
- Fase 3: GUI egui+eframe sobre MapView — dock camadas, canvas MapViewEgui, zoom/pan/pick, tabela atributiva, abrir/salvar projeto, importar; bench documentado com gatilho objetivo p/ Fase 3b condicional; acessibilidade mínima + i18n pt-BR esqueleto; themis security review (path traversal, SQLi bind-only).
## Verification
Por fase: build+deny/audit+SBOM (F0); integração PG16 real (F1); golden round-trip (F2); contrato MapView mock + bench + security review (F3). CI: cargo test --workspace, clippy -D warnings, fmt --check. Suites pesadas opt-in. Coverage: reportar o medido, não inventar.
## Risks
Contaminação clean-room (mitigada por CONTRIBUTING + CI deny-list); alegação falsa de paridade (escopo é tipo-TerraAmazon comportamental); version drift GDAL/PostGIS (pin compose+CI); egui estoura em raster grande (trait MapView + bench gatilho); PG10 como runtime (proibido); modelo IA (definido: GLM-5.3-Flash).
## Acceptance Criteria
Repo compila e CI verde; PG16/PostGIS 3.4 via compose com migrações e CHECK SRID; migrate importa legado→4674; round-trip 5 formatos + derivados; GUI renderiza e edita com teste de contrato MapView; checkpoints atualizados; nenhum artefato legado commitado; themis aprova gates F0 e F3.
## Decisions (Gate 1, 2026-10-16)
Modelo: GLM-5.3-Flash (z.ai) todos os agentes | GPKG: opt-in | Migrate: SQL-text+ogr2ogr | Bench: sintético versionado | Repo: /home/jovyan/testes/terraamazon-cleanroom-rust | Roteamento runtime: general com lente de especialista.
