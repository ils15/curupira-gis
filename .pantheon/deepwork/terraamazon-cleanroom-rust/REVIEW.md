# REVIEW — terraamazon-cleanroom-rust

> **Gate final (@themis, 2026-10-16)** — duplo papel: (A) gate da Fase 3 (GUI
> egui) e (B) review de integração das Fases 0–3 (workflow deepwork Phase
> FINAL). Todos os comandos citados foram **executados de verdade neste host**
> nesta sessão; números só aparecem aqui se medidos aqui ou reproduzidos do
> registro com reexecução.

## Veredito consolidado: **APPROVED_WITH_NOTES**

Nada quebrou e nenhuma alegação material se mostrou falsa — inclusive os
skips "honestos" resistiram à verificação (--nocapture). As notas abaixo são
melhorias e riscos residuais, nenhum bloqueia o MVP. `BLOCKED` ficaria
reservado para quebra real ou mentira material; não foi o caso.

---

## Tabela de fases

| Fase | Escopo | Gate | Evidência principal (re-verificada nesta sessão) |
|---|---|---|---|
| 0 | Scaffolding, licenciamento, barreira clean-room | ✅ APROVADO | dual MIT/Apache + NOTICE + CONTRIBUTING com sha legado só lá; `.gitignore` com deny-list; `deny.toml` executado (ver Parte A.6) |
| 1 | Persistência PostGIS + migrações + `ta-cli migrate` | ✅ APROVADO | harness PG16.10/PostGIS 3.4: pg_integration 6/6 (gated); migrações em banco novo; SRID≠4674 rejeitado; bulk 10k via COPY; migrate SQL-text **e** SHP(ogr2ogr)→4674 re-executados ao vivo |
| 2 | IO geoespacial MVP + derivados | ✅ APROVADO | golden round-trips rodaram no `cargo test --workspace` (ta-gis 43 = count com `--features gdal`); MVT 3/4 **skips honestos confirmados** (probe LIBPROTOBUF, mensagem clara); E2E CLI import→export refeito |
| 3 | GUI egui sobre MapView + bench + security review | ✅ APROVADO (com notas) | build/clippy/fmt/test 124/124; contrato MapView mock (5 métodos); bench re-executado e reprodutível; bind-only nos 3 métodos novos; i18n 87 chaves com teste de completude |

---

## Parte A — Gate Fase 3 (riscos novos)

### A.1 `ta-db` novos métodos (`repo.rs`) — ✅

- `list_with_ids`: uma única query (`SELECT id, ST_AsEWKB(geom), attrs ...
  ORDER BY id`), bind-only (`$1::uuid`), com defesa em profundidade de SRID
  (rejeita EWKB ≠ 4674). Sem zip de queries separadas — a tupla (id, feature)
  não desalinha.
- `update_attrs` / `delete_feature`: `UPDATE ... SET attrs = $2 WHERE id = $1`
  e `DELETE ... WHERE id = $1` — **bind-only**, `Ok(rows_affected > 0)` para
  id ausente. `update_attrs` substitui o JSONB inteiro (documentado no trait,
  não é merge).
- Teste gated `edicao_por_id_update_e_delete` (`tests/pg_integration.rs:372`):
  cobre update existente, update de id inexistente (`Ok(false)`), delete
  existente, delete inexistente, e confere que a linha restante é a certa.
  Sem órfão: o teste usa banco dedicado criado/derrubado pelo harness.
  **Executado no harness desta sessão: 6/6 pg_integration ok.**

### A.2 Diálogo de conexão + DSN — ✅ com 1 nota

- Senha: campo com `.password(true)` (mascarado em tela); a GUI não imprime
  DSN nem senha em evento/log (`db_worker.rs:196-201` manda só o erro + dica).
- DSN montado com `utf8_percent_encode(NON_ALPHANUMERIC)` para usuário e
  senha (`app.rs:166-171`) — senha com `@`/`/` não quebra a URL.
- `TA_DATABASE_URL`: lido com `std::env::var(...).ok()` filtrando vazio
  (`app.rs:76-78`) — **sem panic** em nenhuma forma (ausente, vazio, inválido).
- **NOTA N-1 (média, Layer 2 — segurança):** no **CLI**, a falha de conexão
  ecoa o DSN completo **com senha** (`crates/ta-cli/src/main.rs:41`,
  `format!("conexão falhou ({url}): {e}")`). Reproduzido: apontando para uma
  porta sem servidor, o stderr imprimiu `postgres://ta:ta@localhost:5432/ta`.
  O caminho `migrate` **mascara** (verificado ao vivo: `postgres://ta:***@…`),
  mas `import-vector`/`import-raster`/`list-layers`/`export` não. Com o DSN
  default (`ta:ta`) o vazamento é trivial, mas com `--dsn` de produção vaza
  credencial em logs. Correção sugerida: aplicar o `mask_password` do
  `migrate.rs` no `main.rs`. **Não bloqueia** (ferramenta local, DSN default
  de compose), mas deve entrar no próximo ciclo.

### A.3 `db_worker` / `import_job` — ✅ com 1 nota

- Thread dedicada com **runtime tokio current-thread** dono do `PgPool`
  (`db_worker.rs:161-176`); o pool **nunca** cruza para o estado da UI.
  Sem compartilhamento de pool entre threads; sem deadlock (canais
  `std::sync::mpsc`, `drain()` com `try_recv` não bloqueia o repaint).
- Cancel do import é **cooperativo real entre etapas**: checado após a
  leitura, antes da normalização (`import_job.rs:141-143`); o custo dentro de
  uma etapa (leitura GDAL) é atômico — limitação documentada no cabeçalho do
  módulo, honesta.
- UI não bloqueia no caminho de banco: nenhum blocking call no repaint loop
  vindos do `db_worker`/`import_job` (tudo por canais).
- **NOTA N-2 (baixa/média, Layer 2):** o carregamento de banda raster
  (`raster_view::load_band`) roda **sincronamente na UI thread** em dois
  pontos: ao concluir import raster (`app.rs:336`) e ao trocar de banda no
  inspetor (`panels.rs:248`, dentro do render). É trade-off documentado
  ("carga única"), mas em raster grande (ex.: o TIFF 8192² do bench) a UI
  congela pela duração do downsample GDAL. Sugestão pós-MVP: mover
  `load_band` para thread de fundo com o mesmo padrão mpsc do import.

### A.4 Undo/redo + edição de attrs — ✅ com 1 nota

- Comando stack em memória (`AttrsEdit {layer_idx, feat_idx, old, new}`);
  `push_and_persist` aplica local, empilha e persiste no banco **fire-and-
  forget** via `DbCmd::UpdateAttrs` (`app_state.rs:226-258`).
- Delete: `remove_local` remove local primeiro, limpa undo/redo (índices
  deslocados invalidariam o histórico — conservador e documentado) e manda
  `DbCmd::DeleteFeature` só para `db_id >= 0`.
- **NOTA N-3 (média, Layer 2 — consistência):** se o banco falhar no meio
  (worker morto, conexão caída), `DbEvt::Failed` só alimenta o banner de
  erro — **o estado da UI não é revertido** e o stack de undo/redo permanece.
  Resultado: UI mostra o novo valor / feature removida enquanto o banco
  mantém o antigo / a linha, até o próximo reload (`OpenProject` /
  `LayerRefreshed` realinham; delete falho "reaparece" no reload). Não há
  corrupção de dados nem crash, e o usuário é notificado, mas a política de
  rollback visual em falha de banco merece decisão explícita (reverter local
  no `Failed`, ou marcar a camada como "dirty" e recarregar). Registrar no
  backlog da Fase 4.

### A.5 i18n — ✅

- 87 chaves com guarda de completude: teste valida não-vazio nos 2 idiomas,
  contagem `ALL_KEYS == 87` e divergência pt-BR≠en (tabela en não é cópia
  morta). Rodou verde no `cargo test --workspace`.
- Grep por rótulos hardcoded fora do módulo: restam apenas exceções
  justificáveis — `"URL"` (termo técnico), símbolos `▲ ▼ ✕ +`, nomes de
  idioma no próprio idioma (`Português (BR)`/`English` — prática padrão de
  i18n) e hint_texts com exemplos de caminho/DSN. Nenhum rótulo de UI em
  pt/en fora do `i18n.rs`.

### A.6 Execução própria (host, 2026-10-16) — ✅ tudo verde

| Comando | Resultado |
|---|---|
| `cargo build --workspace` | ✅ Finished dev profile |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ sem warnings |
| `cargo fmt --all --check` | ✅ sem dif |
| `cargo test --workspace` | ✅ **124 passed / 0 failed** (5+9+9+4+6+43+9+33+3+3 por binário de teste) |
| `cargo deny check licenses advisories bans sources` | ✅ `advisories ok, bans ok, licenses ok, sources ok` (warnings apenas de licenças allow-listed não encontradas no grafo) |

### A.7 Bench — ✅ reproduzível, decisão 3b consistente

- `cargo bench -p ta-gui` **re-executado nesta sessão** (dados seed 42
  presentes em `testdata/bench/`, manifest versionado):

| cenário | mean (ms) [publicado] | p95 (ms) [publicado] | max (ms) |
|---|---|---|---|
| 10k_poligonos | 1.85 [2.06] | 2.12 [3.14] | 2.24 |
| 50k_pontos | 2.39 [2.53] | 2.60 [2.59] | 2.73 |
| ambos_mais_raster_1024 | 6.34 [6.96] | 6.96 [8.22] | 7.91 |
| raster_stress_8192 | 0.01 [0.01] | 0.01 [0.01] | 0.02 |

  Todos dentro da variação declarada (±15%, host compartilhado). Decisão 3b
  (**não disparada**) segue consistente: pior p95 medido 6.96 ms ≪ 33 ms.
  O doc `benches/map_render.md` declara honestamente que **GPU não é medida**
  e lista checklist de smoke manual — ressalva mantida (R-GPU abaixo).

---

## Parte B — Review final de integração (Fases 0–3)

### B.8 Suíte completa com banco — ✅

- `bash scripts/test-pg.sh` (cluster efêmero conda: **PostgreSQL 16.10 +
  PostGIS 3.4**, porta 55432): **rc=0**, suíte íntegra verde.
  - `pg_integration`: **6/6 ok** (inclui o novo `edicao_por_id_update_e_delete`).
  - `mvt_integration`: **4/4 ok**, sendo 3 com **skip honesto confirmado com
    `--nocapture`**: "[skip] PostGIS do harness compilou sem protobuf-c —
    ST_AsMVT indisponível (conda-forge); rode na imagem
    postgis/postgis:16-3.4" (probe `postgis_full_version() LIKE
    '%LIBPROTOBUF%'`); 1 teste real client-side (`mvt_zoom_invalido_e_erro`).
- E2E CLI contra o cluster (refeito nesta sessão):
  1. `import-vector --input testdata/fixtures_vector.shp --project
     e2e_review` → **200 features**, `srid 4674`, bbox reportado, 148 ms;
     `SELECT DISTINCT ST_SRID(geom)` no banco = **4674**.
  2. `list-layers` → lista camada vetorial (e tabela de teste MVT).
  3. `export --format gpkg` → **200 features** confirmadas com `ogrinfo -so
     -al` (Feature Count 200; Extent idêntico ao bbox do import).
- `ta-cli migrate` com dump **SQL-text sintético** criado para este review
  (2 polígonos): aplicado com aviso de raw-SQL, **SRID 4674 ✓** nos 2;
  senha mascarada na saída (`postgres://ta:***@…`). Caminho SHP: ogr2ogr
  `-t_srs EPSG:4674` → `ta_import_fixtures_vector`, **200 features, SRID
  4674 ✓**; TIFF pulado por política.
- **Não verificado:** import via GUI headless **com banco** — `app_smoke`
  cobre o import local end-to-end; o caminho de banco usa o mesmo
  `bulk_insert` validado gated, mas não há teste headless GUI↔DB (R-GUI-DB).

### B.9 Clean-room integral — ✅

- `find` por `*.exe/*.zip/*.7z/*.msi/*.tar.gz` fora de `target/.git`:
  **nada**. `grep -ri 50b2bc6a`: **só** `CONTRIBUTING.md` (barreira).
  Menções a "TerraAmazon" no repo: NOTICE/CONTRIBUTING/README apenas como
  referência comportamental negada — corretas.
- `NOTICE.md` e `CONTRIBUTING.md` intactos (declaração clean-room + sha256).
- Manifests de fixtures declaram `clean_room: dados sintéticos
  determinísticos — nada derivado do bundle legado`.
- **Repo sem commits**: `git log` → "does not have any commits yet";
  `git ls-files` → **0 arquivos**. Portanto, trivialmente, **nenhum artefato
  legado commitado** — e o primeiro commit precisa passar pela deny-list do
  `.gitignore` (já configurada: `*.exe *.zip *.7z *.msi *.apk *.dll
  TerraAmazon* legacy/`).
- `cargo deny licenses`: copyleft fora da allow-list = **falha de gate**
  (config mais restritiva que o pedido — correto). Advisories: 1 ignição
  documentada (`RUSTSEC-2026-0192`, ttf-parser unmaintained, transitivo de
  egui, sem vuln ativa).

### B.10 Acceptance criteria (PLAN.md) — item a item

| # | Critério | Estado | Evidência |
|---|---|---|---|
| 1 | Repo compila; CI verde (clippy/fmt); deny+audit ok; SBOM/deny config presente | ⚠️ | build/clippy/fmt/test/deny **verdes medidos aqui**. **CI nunca rodou** (repo sem commits; critério só é verificável no primeiro push). `ci.yml` existe (fmt+clippy+test+deny) mas **não instala `libgdal-dev` nem sobe service PG** — o primeiro run tende a falhar no build do `gdal-sys`/testes gated (risco R8 já registrado no STATUS). **SBOM: inexistente** — nenhum artefato/geração (nenhum `*.spdx`/`*.cdx`, nada no CI); o PLAN pedia SBOM na F0. deny.toml ✅. |
| 2 | PG16/PostGIS 3.4 via harness; migrações aplicam; rejeita SRID≠4674; bulk COPY; `ta-cli migrate` legado→4674 | ✅ | PG **16.10**/PostGIS 3.4 (conda, mesmo contrato do compose `postgis/postgis:16-3.4`); migrações 6/6 gated; teste de rejeição SRID; bulk 10k; **migrate re-executado ao vivo nos 2 caminhos** (SQL-text sintético + ogr2ogr SHP), ambos 4674 ✓ |
| 3 | Round-trip SHP/GeoJSON/GPKG/GeoTIFF/CSV-XY; COG+MVT(skip honesto)+FGB; UnsupportedFormat KML/NetCDF/DWG | ✅ | golden round-trips rodaram no workspace suite (ta-gis 43 = count com gdal: `golden_round_trip_shapefile/geopackage/flatgeobuf`, reprojeção 4326→4674 e 4674→3857→4674); COG gerado no E2E da Fase 2 (não re-executado aqui — unidade de args sem shell ✅); MVT skip honesto **confirmado**; `UnsupportedFormat` com testes unitários |
| 4 | GUI: contrato MapView com mock; pan/zoom/pick; tabela editável; undo/redo; bench publicado com decisão 3b | ✅ | `tests/map_view_contract.rs` exercita os 5 métodos contra mock **e** MapViewEgui; testes de viewport (round-trip tela↔mundo, âncora de zoom, pan, LOD, hit-test); tabela commit em focus-loss com tipagem; `undo_redo_vai_e_volta_com_historico`; bench **re-executado** com números reprodutíveis; decisão 3b não-disparada consistente |
| 5 | Checkpoints atualizados | ✅ | PLAN ✅ · STATUS ✅ (seções por fase com resultados) · DISCOVERY ✅ · REVIEW ✅ (este arquivo). **IMPL-phase-*: não existem como arquivos** — os registros de implementação estão consolidados nas seções "Verificação da Fase N" do STATUS.md (registro oficial desta consolidação). |
| 6 | Nenhum artefato legado commitado | ✅ | repo com **0 commits** / `git ls-files` = 0; deny-list no `.gitignore` pronta para o primeiro commit |

### B.11 Coverage — **não medido**

`cargo-llvm-cov` não está instalado no host; **não instalado** (instrução
expressa). O único número honesto é: 124/124 testes passando. Qualquer
percentual de cobertura aqui seria invenção.

---

## Ressalvas (mantidas e novas)

| ID | Ressalva | Severidade |
|---|---|---|
| R-GPU | Smoke visual/GPU pendente em host com display (checklist em `benches/map_render.md`); headless não mede rasterização GPU — gatilho 3b por raster grande continua **não exercitado** | média, herdada |
| R-MVT | Lane MVT real só na imagem `postgis/postgis:16-3.4` (protobuf-c); no harness conda os 3 testes de rede pulam — nenhum byte protobuf é decodificado no host atual | média, herdada |
| N-1 | CLI ecoa DSN com senha em falha de conexão (`main.rs:41`); `migrate` mascara, o resto não | média |
| N-2 | `load_band` síncrono na UI thread (import raster concluído + troca de banda) — freeze de uma vez em raster grande | baixa/média |
| N-3 | Undo/redo e delete: falha de banco não reverte estado da UI (banner apenas); realinhamento só no reload | média |
| R-CI | CI inexistente na prática (0 commits) e `ci.yml` sem `libgdal-dev`/service PG; ações pinadas por **tag** (`@v4`, `@v1`), não por SHA | média |
| R-SBOM | SBOM não gerado (PLAN F0 pedia); sugerido `cargo cyclonedx-rust`/`cargo auditable` no CI | baixa |
| R-GUI-DB | Import GUI↔DB sem teste headless (app_smoke é local-only) | baixa |
| R-COV | Coverage não medido (ferramenta ausente) | informativa |

## Recomendações pós-MVP (ordenadas)

1. **CI real no primeiro commit**: `apt install libgdal-dev` + service
   `postgis/postgis:16-3.4` (habilita MVT de verdade com protobuf-c) +
   `TA_TEST_PG=1` num job separado; **pinar ações GitHub por SHA**.
2. **Mascarar senha no DSN** em todo o CLI (reusar `mask_password` de
   `migrate.rs` no `main.rs:41`) + teste de regressão.
3. **Política de falha de banco na UI**: reverter local no `DbEvt::Failed`
   de Update/Delete (ou recarregar a camada) e cobrir com teste headless.
4. **SBOM no CI** (`cargo cyclonedx-rust` ou `cargo auditable`) — fecha o
   critério F0 do PLAN.
5. Mover `load_band` para thread de fundo (padrão `import_job`) e cobrir
   import GUI↔DB num smoke gated (`TA_TEST_PG=1`).
6. Rodar o **checklist de smoke manual** (GPU/display) antes de declarar o
   gatilho 3b definitivamente encerrado.
7. `cargo llvm-cov` quando disponível; reportar o medido, sem inventar.

## Conclusão

Fases 0–3 entregues e verificadas de forma independente nesta sessão: suíte
124/124, harness PG verde com o teste de edição novo, bench reprodutível,
clean-room sem achados, migrate duplo caminho funcionando ao vivo. O veredito
**APPROVED_WITH_NOTES** reflete ressalvas reais porém não-bloqueantes — todas
com dono de correção sugerido e nenhuma falsificando claim do STATUS.
