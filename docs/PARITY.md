# Paridade comportamental — matriz e evidências

**Objeto:** fluxo de **monitoramento** do TerraAmazon 7.3.2, reimplementado de
forma independente (clean-room) no Curupira GIS.

**O que esta matriz é.** Um checklist **observável** dos itens IN do plano
aprovado (`.pantheon/deepwork/terraamazon-cleanroom-rust/PLAN-terraamazon-parity.md`,
§3.1), cada um com: estado, comando reproduzível, teste que o prova, arquivos
envolvidos e limitações conhecidas.

**O que esta matriz não é.** Não é uma comparação lado a lado com o produto de
referência nem uma prova de identidade de implementação. As fontes admitidas
são apenas (a) o plano aprovado, (b) evidência observável **do nosso** sistema
(comandos, testes, arquivos) e (c) documentação pública do produto de
referência. Nenhum binário, instalador, descompilação, `hex dump`, offset ou
símbolo interno foi usado — ver `CONTRIBUTING.md` e a seção *Fora por barreira*.

## Como ler

| Estado | Significado |
|---|---|
| ✅ **verificado** | O comportamento foi executado de ponta a ponta **e** existe teste automatizado que o fixa. |
| ⚠️ **parcial** | Parte do comportamento tem evidência; a parte restante está explicitamente listada como não verificada. |
| ❌ **não verificado** | Sem evidência reproduzível; o motivo é declarado. |

Números de teste/cobertura vêm da Fase 6a (ver `STATUS.md`): **369 testes**
em 35 alvos de teste (21 binários de integração + 7 unitários + 7 doc-tests),
cobertura global **73,48%**, `ta-engine` **98,53%**, `ta-db`
**91,97%** (`repo_*.rs` **92,61%**).

## Resumo

| # | Item IN (§3.1) | Estado | Evidência principal |
|---|---|---|---|
| 1 | Projeto e fase | ✅ | `ta-cli phase`; `pg_fase1_repos::phase_repo_create_get_list_delete` |
| 2 | AOI e camadas AOI | ✅ | `ta-cli aoi create`; `pg_monitoring::ciclo_aoi_grade_celulas_e_bloqueio` |
| 3 | Células (grid) e células bloqueadas | ✅ | `ta-cli grid`/`cell`; `ta-engine::grade_da_aoi_fixture_tem_118_celulas` |
| 4 | Classes e grupos | ✅ | `ta-cli class`; `pg_fase2_repos::class_group_e_class_crud_completo` |
| 5 | Regras / rulesets | ✅ | `ta-cli ruleset`/`rule eval`; testes do motor puro |
| 6 | Fila de processos com log por célula | ✅ | `ta-cli task`/`process`/`cell log`; `pg_fase3_queue::run_feliz_ponta_a_ponta_com_proveniencia_e_log` |
| 7 | Classificação básica | ✅ | `ta-cli task run`; `pg_fase3_queue`, teste E2E da GUI |
| 8 | Área | ✅ | `ta-cli area report`; `pg_fase5_area::area_summary_e_coerente` |
| 9 | Import/Export | ✅ | `ta-cli exchange export/import`; `flow_fase4_cli::exchange_round_trip_preserva_contagens_geometria_e_decisao` |
| 10 | Visualização (AOI, células, classes) | ⚠️ | Contrato headless (`map_view_contract`, `pg_viewport_contract`); **sem inspeção humana** |

Todos os testes marcados como `pg_*` são **gated** por `TA_TEST_PG=1`
(`scripts/test-pg.sh`); os demais rodam em `cargo test --workspace`.

---

## 1. Projeto e fase — ✅ verificado

**Comandos (executados):**

```sh
ta-cli import-vector --input testdata/monitoring/aoi.geojson \
    --project parity_flow_demo --layer-name aoi_area
ta-cli phase create --project parity_flow_demo --name "2026-10" --sort-order 1
ta-cli phase list  --project parity_flow_demo
```

Saída observada: `fase criada: 02cef4f3-… projeto=parity_flow_demo bloqueia_celulas=false`
e `02cef4f3-… sort=1 bloqueia_celulas=false 2026-10`.

**Testes:**

```sh
cargo test -p ta-cli --test flow_cli_integration fluxo_cli_cria_fase_aoi_grade_e_bloqueia_celula
cargo test -p ta-db --test pg_fase1_repos phase_repo_create_get_list_delete
cargo test -p ta-cli --test flow_cli_integration fase_nome_duplicado_da_erro_acionavel
```

**Arquivos:** `crates/ta-db/migrations/0005_phases.sql` (`phases`,
`area_settings`, `app_meta`), `crates/ta-db/src/repo_phase.rs`,
`crates/ta-cli/src/flow.rs`, `crates/ta-gui/src/monitoring.rs`.

**Limitações.** A GUI cria e lista fases, mas **não** oferece seleção de fase ao
criar AOI/grade — essa escolha (`--phase`) é feita pela CLI
(`crates/ta-gui/src/monitoring.rs`: “a seleção de fase fica na CLI nesta fase”).
Projeto é criado implicitamente por `import-vector`/`phase create` e removido
por `project delete`; não há renomear projeto na GUI.

---

## 2. AOI e camadas AOI — ✅ verificado

**Comando (executado):**

```sh
ta-cli aoi create --project parity_flow_demo --layer <UUID da camada aoi_area>
```

Saída: `aoi criada: 39b65da5-… projeto=parity_flow_demo camada=aoi_area (<uuid>)`.

**Testes:**

```sh
cargo test -p ta-db --test pg_monitoring ciclo_aoi_grade_celulas_e_bloqueio
cargo test -p ta-cli --test flow_cli_integration fluxo_cli_cria_fase_aoi_grade_e_bloqueia_celula
cargo test -p ta-db --test pg_fase1_repos phase_repo_create_get_list_delete
```

**Arquivos:** `crates/ta-db/migrations/0006_aoi_cells.sql` (`aoi_layers`),
`crates/ta-db/src/repo_aoi.rs`, `crates/ta-flow/src/aoi.rs`.

**Limitações.** A AOI é uma **camada vetorial existente** eleita como área de
interesse; não há desenho/edição de AOI no canvas. A AOI pode ter buracos
(multi/anel) — o recorte os respeita (`ta-engine::buraco_da_aoi_exclui_celulas`),
mas a validação é por polígono simples/multipolygon 2D; Z/M é achatado na
importação.

---

## 3. Células (grid) e células bloqueadas — ✅ verificado

**Comandos (executados):**

```sh
ta-cli grid create --aoi-layer <UUID aoi_layers> --resolution 0.005 --name grade-4674
ta-cli grid info   --cell-layer <UUID cell_layers>
ta-cli cell block  --cell-layer <UUID> --cell-id 0_1 --reason "nuvem"
ta-cli cell list-blocked --cell-layer <UUID>
```

Saídas observadas: `grade criada: … resolucao=0.005 (graus) srid=4674 células: 118`;
`células: 118 bloqueadas: 0`; `célula bloqueada: 0_1`; `0_1 nuvem`.

**Testes:**

```sh
cargo test -p ta-engine grade_da_aoi_fixture_tem_118_celulas
cargo test -p ta-engine fronteira_da_aoi_segue_intervalo_semiaberto
cargo test -p ta-engine buraco_da_aoi_exclui_celulas
cargo test -p ta-engine grade_grande_demais_e_rejeitada_sem_alocar
cargo test -p ta-db --test pg_grid_srid grade_metrica_em_5880_persiste_celulas_em_4674
```

**Arquivos:** `crates/ta-engine/src/grid.rs` (`generate_grid`,
`candidate_count`, `MAX_CELLS`), migrações
`crates/ta-db/migrations/0006_aoi_cells.sql`,
`crates/ta-db/migrations/0007_grid_srid.sql`,
`crates/ta-db/migrations/0008_cell_validity.sql`,
`crates/ta-db/src/repo_cell.rs`.

**Limitações.** A grade é regular, alinhada a uma origem, e inclui a célula
pelo **centro**; o envelope é **semiaberto** `[min, max)` — a aresta de máximo
fica fora (documentado no plano §7). Há **dois** limites distintos: o **teto do
motor** é `MAX_CELLS = 5 000 000` (`crates/ta-engine/src/grid.rs:64`, proteção
de memória do crate puro, independente de política); o **teto de negócio**
default é `projects.max_cells = 1 000 000`, configurável por projeto. Nenhum
dos dois foi exercitado em escala real (ver *O que NÃO podemos afirmar*).
Bloqueio é por célula inteira (não há máscara parcial).

---

## 4. Classes e grupos de classes — ✅ verificado

**Comandos (executados):**

```sh
ta-cli class group create --project parity_flow_demo --name cobertura
ta-cli class create --project parity_flow_demo --name floresta \
    --color "#0A7D2C" --group cobertura
ta-cli class create --project parity_flow_demo --name desmatamento \
    --color "#C62828" --group cobertura
ta-cli class list --project parity_flow_demo
```

Saída observada: `grupo criado: … cobertura`; o bloco de comandos acima tem
**duas** chamadas `class create` (e duas linhas `classe criada: …` com
`cor=#…`); a terceira classe do projeto de demonstração já havia sido criada
na execução real do fluxo e aparece apenas na listagem. A listagem mostra
grupo, atividade, `sort` e cor.

**Testes:**

```sh
cargo test -p ta-db --test pg_fase2_repos class_group_e_class_crud_completo
cargo test -p ta-db --test pg_fase2_repos parent_id_ciclo_e_auto_pai_sao_rejeitados
cargo test -p ta-cli --test flow_fase2_cli cli_crud_de_grupos_classes_e_rascunhos
cargo test -p ta-db --test pg_fase2_repos update_partial_preserva_campos_ausentes
```

**Arquivos:** `crates/ta-db/migrations/0009_classes.sql`
(`classes`, `class_groups`, `draft_layers`), `crates/ta-db/src/repo_class.rs`.

**Limitações.** Hierarquia de classes é validada contra ciclos, mas não há
editor visual de árvore de classes; a cor é `#RRGGBB` (sem paleta temática).
`class delete` com regras exige `--force` (pré-checagem mostra a contagem) —
caminho de saída explícito, não cascata silenciosa.

---

## 5. Regras / rulesets — ✅ verificado

**Comandos (executados):**

```sh
ta-cli ruleset create --project parity_flow_demo --name monitoramento-v1
ta-cli rule add --ruleset <UUID> --class floresta --name floresta_ndvi \
    --priority 10 --when "ndvi >= 0.6"
ta-cli rule add --ruleset <UUID> --class desmatamento --name desmat_ndvi \
    --priority 5 --when "ndvi < 0.3 and nbr < 0.2"
ta-cli ruleset activate --project parity_flow_demo --name monitoramento-v1
ta-cli rule eval --ruleset <UUID> --obs "ndvi=0.7,nbr=0.05"
```

Saída observada: `decisão: classe=floresta … regra=floresta_ndvi prioridade=10`;
com `ndvi=0.2,nbr=0.1` → `classe=desmatamento … regra=desmat_ndvi`.

**Testes:**

```sh
cargo test -p ta-engine prioridade_maior_vence_independente_da_ordem
cargo test -p ta-engine avaliacao_e_deterministica_inclusive_com_ordem_trocada
cargo test -p ta-engine medida_ausente_torna_a_comparacao_falsa_e_e_registrada
cargo test -p ta-engine sintaxe_invalida_traz_a_posicao
cargo test -p ta-cli --test flow_fase2_cli cli_eval_decide_por_prioridade_desempata_por_nome_e_explica_ausentes
cargo test -p ta-db --test pg_fase2_repos rule_exige_classe_do_mesmo_projeto_e_ruleset_remove_em_cascata
```

**Arquivos:** `crates/ta-engine/src/rule.rs` (`parse_predicate`, `evaluate`),
`crates/ta-engine/src/batch.rs` (`classify_batch`), migrações
`crates/ta-db/migrations/0010_rules.sql` e
`crates/ta-db/migrations/0011_rule_class_restrict.sql`,
`crates/ta-db/src/repo_ruleset.rs`.

**Limitações.** O predicado é **mínimo e observável**: comparações numéricas
com medidas nomeadas, `and`/`or`/`not` e parênteses; **não** há predicados
geoespaciais (interseção, distância), temporais ou espectrais embutidos. Regra
sem medida nunca casa (e a ausência é reportada). Rulesets são versionados por
nome; no máximo um ativo por projeto.

---

## 6. Fila de processos com log por célula — ✅ verificado

**Comandos (executados):**

```sh
ta-cli measure import-csv --cell-layer <UUID> \
    --input testdata/monitoring/medidas.csv
ta-cli task create --project parity_flow_demo --cell-layer <UUID> \
    --name classificacao-paridade classify
ta-cli task run <UUID da task>
ta-cli task show <UUID da task>
ta-cli process list --task <UUID> --status done
ta-cli cell log --cell-layer <UUID> --cell-id 1_1
```

Saídas observadas: `task criada: … processos enfileirados: 117 novos / 117
células elegíveis (bloqueadas fora)`; `lote ok: 117 células (3 linhas de
rascunho, 117 logs)`; `status da task: done`; log por célula com classe e regra.

**Testes:**

```sh
cargo test -p ta-db --test pg_fase3_queue run_feliz_ponta_a_ponta_com_proveniencia_e_log
cargo test -p ta-db --test pg_fase3_queue retomada_com_limite_nao_reprocessa_done
cargo test -p ta-db --test pg_fase3_queue erro_controlado_gera_last_error_e_retry_recupera
cargo test -p ta-cli --test flow_fase3_cli fluxo_create_run_show_com_retomada_por_limite
cargo test -p ta-cli --test flow_fase3_cli task_cancel_e_cooperativo_e_mensagens_acionaveis
```

**Arquivos:** `crates/ta-flow/src/queue.rs` (`create_task`, `run_task`,
`cancel_task`, `retry_task`, `RunEvent`), migração
`crates/ta-db/migrations/0012_queue.sql`
(`tasks`, `processes`, `cell_log`), `crates/ta-db/src/repo_task.rs`.

**Limitações.** Um **único executor cooperativo** processa lotes
transacionais; não há concorrência distribuída, fila externa ou agendamento.
Cancelamento é **entre lotes** (o lote em voo termina). `done` nunca é
reprocessado; `retry` repõe apenas `error → pending`. Células bloqueadas ficam
fora do enfileiramento.

---

## 7. Classificação básica — ✅ verificado

**Comandos (executados):** `measure import-csv` → `task create … classify` →
`task run` → `draft list` → `cell log`/`process list` (mesmos da seção 6).

Saída observada: 117 células processadas (todas as elegíveis), **3 decisões**
gravadas — o número de decisões não é o número de células medidas: depende de
as regras casarem. Na reprodução do Themis, 3 células medidas produziram **2**
decisões; nesta execução, 3. 0 erros, `draft` em estado `draft`, log por
célula.

**Testes:**

```sh
cargo test -p ta-gui --test pg_gui_monitoring worker_classifica_duas_celulas_e_grava_duas_decisoes
cargo test -p ta-gui --test pg_gui_monitoring fluxo_completo_pela_gui_bate_com_a_cli
cargo test -p ta-engine lote_e_deterministico
cargo test -p ta-engine prioridade_maior_vence_independente_da_ordem
```

**Arquivos:** `crates/ta-engine/src/batch.rs`, `crates/ta-flow/src/queue.rs`,
`crates/ta-db/migrations/0013_drafts_measures.sql` (`draft_cells`).

**Limitações.** A classificação é **por regras sobre medidas importadas**.
O produto **não** calcula índices espectrais, não lê pixels e não classifica
imagem: a medida (`ndvi`, `nbr`, …) entra por CSV/`measure set` ou pelo
exchange. Portanto a paridade é do **fluxo de decisão**, não de processamento
de imagem.

---

## 8. Área — ✅ verificado

**Comando (executado):**

```sh
ta-cli area report --cell-layer <UUID> --draft "rascunho monitoramento-v1"
```

Saída observada: `srs de cálculo: EPSG:5880 (metros)`; `total: 3627.54 ha
(118 células)`; `bloqueada: 30.74 ha (1 células)`; `disponível: 3596.80 ha`;
`desmatamento 1 células 30.74 ha`; `floresta 2 células 61.48 ha`.

**Testes:**

```sh
cargo test -p ta-db --test pg_fase5_area area_summary_e_coerente
cargo test -p ta-db --test pg_fase5_area ordem_de_grandeza_5880_vs_4674
cargo test -p ta-db --test pg_fase5_area srs_para_area_e_validado
cargo test -p ta-flow --test pg_area_report report_usa_o_default_5880_e_traz_por_classe
cargo test -p ta-cli --test flow_fase5_area_cli area_report_legivel_com_por_classe
cargo test -p ta-cli --test flow_fase5_area_cli area_report_json_e_parseavel
```

**Arquivos:** `crates/ta-db/src/repo_area.rs`, `crates/ta-flow/src/area.rs`,
`crates/ta-core/src/area.rs`, tabela `area_settings` (`0005_phases.sql`).

**Limitações.** A área é calculada no SRS **métrico** de
`area_settings.srs_id` (default **EPSG:5880**, Policônica SIRGAS 2000) e
**recusa** SRS geográfico; não é área geodésica exata. Para grades muito
extensas e distantes do meridiano central, a projeção introduz distorção — o
teste fixa apenas a **ordem de grandeza** contra graus, não exatidão absoluta.
A unidade exibida é hectare (10 000 m²).

---

## 9. Import/Export — ✅ verificado

**Comandos (executados):**

```sh
ta-cli exchange export --project parity_flow_demo --out /tmp/opencode/parity_exchange.json
ta-cli exchange import --in /tmp/opencode/parity_exchange.json --into parity_flow_demo_copy
ta-cli area report --cell-layer <grade do projeto importado> --draft "rascunho monitoramento-v1"
ta-cli project delete --project parity_flow_demo_copy --yes
```

Saída observada: export `35155 bytes` com `fases=1 aoi=1 features=1 grades=1
celulas=118 bloqueadas=1 grupos=1 classes=3 rulesets=1 regras=2 rascunhos=1
decisoes=3 medidas=8`; import com **as mesmas contagens**; área do projeto
importado **idêntica** (`3627.54 / 30.74 / 3596.80 ha`).

**Testes:**

```sh
cargo test -p ta-cli --test flow_fase4_cli exchange_round_trip_preserva_contagens_geometria_e_decisao
cargo test -p ta-cli --test flow_fase4_cli import_falho_nao_deixa_projeto_parcial_e_retry_funciona
cargo test -p ta-gui --test pg_gui_monitoring exchange_usa_o_binario_da_cli_e_falha_com_caminho_inexistente
cargo test -p ta-gis --features gdal,geojson,csv --test round_trip golden_round_trip_geojson
cargo test -p ta-gis --features gdal,geojson,csv --test round_trip golden_round_trip_csv_xy
```

**Atenção às features.** `crates/ta-gis/tests/round_trip.rs` tem
`#![cfg(feature = "gdal")]` e gates por `geojson`/`csv`: **sem** `--features`
o binário compila **zero testes** e `cargo test` sai 0 (falso-verde). Os
comandos acima habilitam o binário inteiro (9 testes) e são os reproduzíveis.

**Arquivos:** `crates/ta-gis/src/exchange.rs` (formato
`curupira-monitoring-exchange v1`), `crates/ta-cli/src/flow_exchange.rs`,
`crates/ta-cli/src/import.rs`, `crates/ta-gis/src/{reader,writer,normalizer}.rs`.

**Limitações.** O exchange é um **formato próprio** (JSON versionado), não um
formato do produto de referência; importação cria **projeto novo** (sem merge
nesta fase) e é atômica em transação única. O exchange cobre **vetor,
medidas, decisões, regras e cenas** — **não** carrega raster/pixels. Também
existem import/export vetoriais avulsos (`import-vector`, `export` para
SHP/GeoJSON/GPKG/FGB/CSV), com round-trip por formato testado em
`crates/ta-gis/tests/round_trip.rs`.

---

## 10. Visualização (AOI, células, classes no canvas) — ⚠️ parcial

**O que está verificado (headless):**

```sh
cargo test -p ta-gui --test map_view_contract overlay_colorido_por_classe_e_bloqueio_e_pick_da_celula
cargo test -p ta-gui --test map_view_contract egui_headless_render_produz_shapes_sem_gpu
cargo test -p ta-gui --test map_view_contract pick_da_feature_certa_apos_render_headless
cargo test -p ta-db --test pg_viewport_contract contrato_da_viewport_de_celulas
cargo test -p ta-gui --test pg_gui_monitoring comandos_de_monitoramento_da_gui_ponta_a_ponta
cargo test -p ta-gui --test pg_gui_monitoring fluxo_completo_pela_gui_bate_com_a_cli
```

Esses testes provam: cor por classe, precedência de bloqueio sobre classe,
célula sem decisão em cinza, legenda derivada das células visíveis, `pick` de
célula/feição, recorte de viewport por bbox com `LIMIT` de 5 000 (clamp) e
aviso de truncamento, e que a GUI dispara o **mesmo** fluxo da CLI com
resultados iguais.

**Arquivos:** `crates/ta-gui/src/monitoring.rs` (`overlay`, `cell_fill`,
`legend_items`, `needs_reload`, `window`, `draw_legend`),
`crates/ta-gui/src/map_view.rs` (`CellOverlay`, `cell_at`),
`crates/ta-db/src/repo_cell.rs` (`viewport_on`).

**O que NÃO está verificado (e por quê).** Não houve **inspeção visual humana
por display**: o host de verificação não tem `DISPLAY`/`WAYLAND_DISPLAY` e o
binário encerra com `neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is
set`. Portanto não afirmamos: legibilidade, ergonomia, fidelidade visual,
posição de rótulos, nem comportamento em GPU. A verificação é de **contrato**
(estado → shapes/cores/pick), não de aparência.

**Limitações adicionais.** A GPU não é medida (o bench headless mede tesselação
CPU). Overlay carrega no máximo 5 000 células por viewport; grades maiores são
truncadas com aviso (não é paginação).

---

## OUT — explicitamente fora do escopo (§3.2)

| Item OUT | Estado no repositório |
|---|---|
| Raster pesado (álgebra/processamento de pixels) | **não implementado**. `import-raster` registra apenas **metadados** (dimensões, geotransform, bbox, CRS); pixels nunca vão ao PostGIS. `--cog` deriva um COG via GDAL externo. |
| DTM | **não implementado** (sem comando, tabela ou crate). |
| Gráficos | **não implementado** (sem módulo de chart). |
| Layout de impressão | **não implementado**. |
| Servidor WMS/WCS | **não implementado**. Existe somente **cliente** WMS 1.3.0 opt-in (`ta-gis` feature `wms`, cache em disco) — não é servidor e não faz parte do fluxo de monitoramento. |
| Geocoding | **não implementado**. |
| Plugin manager real | **não implementado**: existe um diálogo **stub** (`PluginsStub` em `crates/ta-gui/src/i18n.rs`) que apenas informa a indisponibilidade; não há carregamento de plugins. |
| Multi-usuário | **não implementado** (aplicação desktop single-user; o banco tem autenticação, sem colaboração/controle de concorrência entre usuários). |
| Bookmarks | **não implementado**. |
| Image servers | **não implementado** (só o cliente WMS com cache). |

Esses itens não são “parcialmente feitos”: não há comando, tabela ou teste que
os exponha. A única proximidade é o **cliente** WMS, que o plano classifica
como OUT na forma de **servidor**.

## Fora por barreira (não por escopo, §3.3)

Qualquer insumo derivado do binário/instalador legado ou de engenharia reversa
está **proibido** (`CONTRIBUTING.md`, linhas 16–30). A barreira é aplicada por
revisão de PR e pelo `NOTICE.md`.

| Proibido | Por quê |
|---|---|
| Inspecionar binário, instalador ou pacote legado (e qualquer extração/listagem/strings/asset derivado) | é engenharia reversa do produto legado; a especificação só pode vir do comportamento público. |
| Descompilar, desassemblar, tracejar, depurar ou fazer RE de binário/módulo | idem; qualquer derivação interna contamina o clean-room. |
| Copiar código, pseudocódigo, recursos, ícones, strings ou layout | viola a independência da reimplementação e a licença do produto legado. |
| Offsets/endereços, `hex dumps`, assembly/bytecode, símbolos internos, hashes do pacote | itens de **rejeição automática** de PR (lista objetiva do `CONTRIBUTING.md`). |

Nada disso existe neste repositório: a matriz e os documentos citam apenas o
plano aprovado, comandos/testes/arquivos do nosso sistema e, no máximo,
documentação pública do produto de referência.

## O que NÃO podemos afirmar

1. **Paridade visual por inspeção humana.** Não houve execução com display;
   a visualização é verificada por contrato headless (seção 10).
2. **Paridade de comportamento exato com o produto de referência** além do que
   é público. Não fizemos comparação lado a lado; a especificação é
   comportamental e o escopo é o §3.1.
3. **Desempenho em escala.** Não medimos 5 000 000 de células (teto do
   motor, `MAX_CELLS`) nem 1 000 000 (teto de negócio default de
   `projects.max_cells`), nem AOIs continentais. O bench existente é de `ta-db`
   com 10 000 feições (`benches/BASELINE.md`): `bulk 511,5 ms`,
   `bbox 12,57 ms`, `list 28,21 ms`. **MVT não foi medido** neste host
   (PostGIS conda sem protobuf-c).
4. **Cobertura de `ta-gui`/`ta-cli` acima de 85%.** A meta de 85% vale para
   `ta-engine` (98,53%) e `repo_*.rs` (92,61%); `ta-gui` tem **64,79%** e
   `ta-cli` **72,02%**. A meta global é 70% (atingida: 73,48%).
5. **Exatidão absoluta de área.** A área é projetada (EPSG:5880), não
   geodésica; o teste fixa ordem de grandeza, não exatidão.
6. **Classificação de imagem.** O produto decide classe por regras sobre
   medidas importadas; não há processamento espectral/pixel no app.

## Defeitos conhecidos

1. **`project delete` — atribuição de `draft_layers` (corrigido na Fase 6).**
   O relatório contava `draft_layers=0` e o total ficava 1 abaixo do preview
   quando o projeto tinha rascunho ligado à grade: `cell_layers` era apagada
   antes e a FK `draft_layers.cell_layer_id` é `ON DELETE CASCADE`. A ordem
   foi corrigida (`draft_layers` → … → `cell_layers`) e há regressão em
   `flow_fase4_cli::project_delete_ordena_e_zera_banco`, que exige
   `draft_layers=1` e preview == total do delete.
2. **Dívidas de teste**: dedup de helpers (n7/n8/n13) ainda aberto.

## Como reproduzir

```sh
# 0) binários do próprio build (sem depender do PATH do shell)
export PATH="$PWD/target/debug:$PATH"

# 1) ambiente (banco + schema + demo), idempotente
bash scripts/dev-up.sh

# 2) suíte completa (unitária + integração sem banco)
cargo test --workspace

# 3) suíte com PostGIS real (gated)
TA_TEST_PG=1 TA_PG_DSN=postgres://ta:ta@127.0.0.1:55432/ta \
    cargo test --workspace

# 4) fluxo CLI (com o banco do dev-up; troque <ids> pelos impressos)
ta-cli import-vector --input testdata/monitoring/aoi.geojson --project demo2
ta-cli phase create --project demo2 --name "2026-10"
ta-cli list-layers --project demo2            # UUID da camada AOI
ta-cli aoi create --project demo2 --layer <UUID camada>
ta-cli grid create --aoi-layer <UUID aoi> --resolution 0.005
ta-cli class create --project demo2 --name floresta --color "#0A7D2C"
ta-cli ruleset create --project demo2 --name regras
ta-cli rule add --ruleset <UUID> --class floresta --name f --when "ndvi >= 0.6"
ta-cli ruleset activate --project demo2 --name regras
ta-cli measure import-csv --cell-layer <UUID grade> \
    --input testdata/monitoring/medidas.csv
ta-cli task create --project demo2 --cell-layer <UUID grade> classify
ta-cli task run <UUID task> && ta-cli task show <UUID task>
ta-cli area report --cell-layer <UUID grade> --draft "rascunho regras"
ta-cli cell block --cell-layer <UUID grade> --cell-id 0_1 --reason nuvem
ta-cli exchange export --project demo2 --out /tmp/demo2.json
ta-cli exchange import --in /tmp/demo2.json --into demo2_copy

# 5) GUI (precisa de display)
bash scripts/dev-up.sh --gui
# ou: TA_DATABASE_URL=postgres://ta:ta@127.0.0.1:55432/ta cargo run -p ta-gui
```

## Registro de verificação deste documento

Executado em 2026-10-19, host Linux (sem display), banco PostgreSQL 16.10 +
PostGIS 3.4.3 (porta 55432):

| Check | Resultado |
|---|---|
| `cargo fmt --all -- --check` | ✅ |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ |
| `cargo test --workspace` | ✅ 369 testes / 35 alvos (21 integração + 7 unit + 7 doc-tests) |
| `TA_TEST_PG=1 TA_PG_DSN=… cargo test --workspace` | ✅ 369/369, 0 ignored; **3 testes de MVT auto-pulam** em hosts sem protobuf-c (a prova real de MVT é o job `linux-pg` do CI) |
| `bash scripts/dev-up.sh` (2×) | ✅ exit 0, doctor 5 ok / 1 aviso (MVT), seed idempotente |
| `ta-cli doctor` | ✅ 5 ok, 1 aviso (MVT sem protobuf-c), 0 falhas |
| `cargo deny check` | ✅ advisories/bans/licenses/sources ok (2 avisos de licença não usada, transitivos: `CC0-1.0`, `Unicode-DFS-2016`) |
| `cargo test -p ta-gis --features gdal,geojson,csv --test round_trip` | ✅ 9/9 (sem `--features`: 0 testes, falso-verde) |
| `ta-cli measure import-csv … testdata/monitoring/medidas.csv` | ✅ `lidas=5 gravadas=5 ignoradas=0` |
| `ta-cli project delete` (projeto com 1 rascunho) | ✅ preview `total=128 draft_layers=1` == delete `total=128 draft_layers=1` |
| Fluxo CLI completo (seções 1–9) | ✅ executado; números acima são a saída real |

As contagens e números citados (118 células, 117 processos, 3627.54 ha,
`35155 bytes`) são **desta execução**; outro dataset produz outros números.
