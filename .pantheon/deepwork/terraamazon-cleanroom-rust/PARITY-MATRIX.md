# PARITY-MATRIX — versão de trabalho (Fase 6b)

> **Documento de trabalho.** A versão para o revisor é `docs/PARITY.md`.
> Esta versão guarda a mesma matriz com os **transcritos crus** da execução e o
> índice teste→arquivo, para auditoria interna. Fontes admitidas: plano
> aprovado (§3.1/§3.2/§3.3), evidência do **nosso** sistema e documentação
> pública. Nenhum material de engenharia reversa.

- Data: 2026-10-19
- Branch: `feat/paridade-monitoramento` (HEAD `75d921d`; Fases 6a+6b na árvore)
- Banco: PostgreSQL 16.10 + PostGIS 3.4.3 (conda, porta 55432)
- Host: Linux sem `DISPLAY`/`WAYLAND_DISPLAY`

## 1. Matriz (IN)

Caminhos de produção na coluna final são relativos a `crates/<crate>/src/`;
arquivos numerados (`0005_…` etc.) ficam em `crates/ta-db/migrations/`.

| # | Item | Estado | Teste de contrato | Arquivo de produção |
|---|---|---|---|---|
| 1 | Projeto e fase | ✅ | `flow_cli_integration::fluxo_cli_cria_fase_aoi_grade_e_bloqueia_celula`; `pg_fase1_repos::phase_repo_create_get_list_delete` | `repo_phase.rs`, `0005_phases.sql`, `ta-flow/src/phase.rs` |
| 2 | AOI e camadas AOI | ✅ | `pg_monitoring::ciclo_aoi_grade_celulas_e_bloqueio`; `pg_fase1_repos::phase_repo_create_get_list_delete` | `repo_aoi.rs`, `0006_aoi_cells.sql`, `ta-flow/src/aoi.rs` |
| 3 | Células e bloqueadas | ✅ | `ta-engine::grade_da_aoi_fixture_tem_118_celulas`, `fronteira_da_aoi_segue_intervalo_semiaberto`, `buraco_da_aoi_exclui_celulas`; `pg_grid_srid::grade_metrica_em_5880_persiste_celulas_em_4674` | `ta-engine/src/grid.rs`, `repo_cell.rs`, `0006`–`0008` |
| 4 | Classes e grupos | ✅ | `pg_fase2_repos::class_group_e_class_crud_completo`, `parent_id_ciclo_e_auto_pai_sao_rejeitados`; `flow_fase2_cli::cli_crud_de_grupos_classes_e_rascunhos` | `repo_class.rs`, `0009_classes.sql`, `ta-flow/src/` (ruleset/class) |
| 5 | Regras / rulesets | ✅ | `ta-engine::prioridade_maior_vence_independente_da_ordem`, `avaliacao_e_deterministica_inclusive_com_ordem_trocada`, `sintaxe_invalida_traz_a_posicao`; `flow_fase2_cli::cli_eval_decide_por_prioridade_desempata_por_nome_e_explica_ausentes` | `ta-engine/src/rule.rs`, `ta-engine/src/batch.rs`, `repo_ruleset.rs`, `0010`/`0011` |
| 6 | Fila + log por célula | ✅ | `pg_fase3_queue::run_feliz_ponta_a_ponta_com_proveniencia_e_log`, `retomada_com_limite_nao_reprocessa_done`, `erro_controlado_gera_last_error_e_retry_recupera`; `flow_fase3_cli::fluxo_create_run_show_com_retomada_por_limite` | `ta-flow/src/queue.rs`, `repo_task.rs`, `0012_queue.sql` |
| 7 | Classificação básica | ✅ | `pg_gui_monitoring::worker_classifica_duas_celulas_e_grava_duas_decisoes`, `fluxo_completo_pela_gui_bate_com_a_cli`; `ta-engine::lote_e_deterministico` | `ta-engine/src/batch.rs`, `ta-flow/src/queue.rs`, `0013_drafts_measures.sql` |
| 8 | Área | ✅ | `pg_fase5_area::area_summary_e_coerente`, `ordem_de_grandeza_5880_vs_4674`, `srs_para_area_e_validado`; `pg_area_report::report_usa_o_default_5880_e_traz_por_classe` | `repo_area.rs`, `ta-flow/src/area.rs`, `ta-core/src/area.rs` |
| 9 | Import/Export | ✅ | `flow_fase4_cli::exchange_round_trip_preserva_contagens_geometria_e_decisao`, `import_falho_nao_deixa_projeto_parcial_e_retry_funciona`; `ta-gis round_trip.rs` | `ta-gis/src/exchange.rs`, `flow_exchange.rs`, `import.rs` |
| 10 | Visualização | ⚠️ | `map_view_contract::overlay_colorido_por_classe_e_bloqueio_e_pick_da_celula`; `pg_viewport_contract::contrato_da_viewport_de_celulas`; `pg_gui_monitoring::comandos_de_monitoramento_da_gui_ponta_a_ponta` | `ta-gui/src/monitoring.rs`, `map_view.rs`, `repo_cell.rs::viewport_on` |

## 2. Transcritos da execução real (fluxo `parity_flow_demo`)

Projeto criado do zero e apagado ao final. Comandos e saídas (recortadas):

```
$ ta-cli import-vector --input testdata/monitoring/aoi.geojson \
    --project parity_flow_demo --layer-name aoi_area
import-vector ok: … → camada `aoi_area` (41682f3a-…) no banco
  features: 1 (inseridas 1)   srid: Some(4674) → 4674   tempo: 212 ms

$ ta-cli phase create --project parity_flow_demo --name "2026-10" --sort-order 1
fase criada: 02cef4f3-…  projeto=parity_flow_demo  bloqueia_celulas=false

$ ta-cli aoi create --project parity_flow_demo --layer 41682f3a-…
aoi criada: 39b65da5-…  projeto=parity_flow_demo  camada=aoi_area (41682f3a-…)

$ ta-cli grid create --aoi-layer 39b65da5-… --resolution 0.005 --name grade-4674
grade criada: 4eda4e51-…  nome=grade-4674  resolucao=0.005 (graus)  srid=4674
  células: 118 (inseridas agora: 118)

$ ta-cli cell block --cell-layer 4eda4e51-… --cell-id 0_1 --reason "nuvem"
célula bloqueada: 0_1  camada=4eda4e51-…

$ ta-cli class group create --project parity_flow_demo --name cobertura
grupo criado: e143ba60-…  projeto=parity_flow_demo  cobertura
$ ta-cli class create … floresta/desmatamento/hidrografia
classe criada: … cor=#0A7D2C / #C62828 / #1565C0

$ ta-cli ruleset create --project parity_flow_demo --name monitoramento-v1
ruleset criado: 9abc666d-…  versao=1
$ ta-cli rule add --ruleset 9abc666d-… --class floresta --name floresta_ndvi --priority 10 --when "ndvi >= 0.6"
$ ta-cli rule add --ruleset 9abc666d-… --class desmatamento --name desmat_ndvi --priority 5 --when "ndvi < 0.3 and nbr < 0.2"
$ ta-cli ruleset activate --project parity_flow_demo --name monitoramento-v1
ruleset ativo: 9abc666d-…  versao=1

$ ta-cli rule eval --ruleset 9abc666d-… --obs "ndvi=0.7,nbr=0.05"
decisão: classe=floresta  regra=floresta_ndvi  prioridade=10
$ ta-cli rule eval --ruleset 9abc666d-… --obs "ndvi=0.2,nbr=0.1"
decisão: classe=desmatamento  regra=desmat_ndvi  prioridade=5

$ ta-cli measure import-csv --cell-layer 4eda4e51-… --input testdata/monitoring/medidas.csv
medidas importadas: lidas=8  gravadas=8  ignoradas=0

$ ta-cli task create --project parity_flow_demo --cell-layer 4eda4e51-… classify
task criada: 043ca43a-…  status=pending  ruleset=monitoramento-v1 (v1)
processos enfileirados: 117 novos / 117 células elegíveis (bloqueadas fora)
$ ta-cli task run 043ca43a-…
lote ok: 117 células (3 linhas de rascunho, 117 logs)
run concluído: processadas=117 erros=0 … status da task: done

$ ta-cli cell log --cell-layer 4eda4e51-… --cell-id 1_1
2026-10-19 …Z  info  classe=7c9ab0d0-… regra=ba9a1f5c-…

$ ta-cli area report --cell-layer 4eda4e51-… --draft "rascunho monitoramento-v1"
srs de cálculo: EPSG:5880 (metros)
total: 3627.54 ha (118 células)
bloqueada: 30.74 ha (1 células)   disponível: 3596.80 ha
  desmatamento 1 células 30.74 ha
  floresta     2 células 61.48 ha

$ ta-cli exchange export --project parity_flow_demo --out /tmp/…json
exchange exportado: … (35155 bytes)
fases=1 aoi=1 features=1 grades=1 celulas=118 bloqueadas=1
grupos=1 classes=3 rulesets=1 regras=2 rascunhos=1 decisoes=3 medidas=8 cenas=0

$ ta-cli exchange import --in /tmp/…json --into parity_flow_demo_copy
exchange importado: projeto `parity_flow_demo_copy` (788c3ecf-…)
… mesmas contagens …
$ ta-cli area report --cell-layer <grade do import> --draft "rascunho monitoramento-v1"
total: 3627.54 ha  bloqueada: 30.74 ha  disponível: 3596.80 ha
  desmatamento 1 células 30.74 ha
  floresta     2 células 61.48 ha
```

### Prova da correção do `project delete` (Fase 6, 2026-10-19)

Projeto `parity_gate_proof` criado do zero (AOI + grade de 118 células +
rascunho + 5 medidas de `testdata/monitoring/medidas.csv`):

```
$ ta-cli measure import-csv --cell-layer ca43028f-… --input testdata/monitoring/medidas.csv
medidas importadas: lidas=5  gravadas=5  ignoradas=0

$ ta-cli project delete --project parity_gate_proof          # sem --yes
erro: recusado sem --yes: apagar o projeto `parity_gate_proof` remove 128 linha(s):
  draft_cells=0 rules=0 rulesets=0 cell_log=0 processes=0 tasks=0
  cell_measures=5 blocked_cells=0 cells=118 cell_layers=1 draft_layers=1
  features=1 layers=1 aoi_layers=1 project_scenes=0 classes=0 class_groups=0 phases=0

$ ta-cli project delete --project parity_gate_proof --yes
projeto apagado: parity_gate_proof (…)
total=128
draft_cells=0 … cell_layers=1 draft_layers=1 … phases=0
```

Preview e delete coincidem (`total=128`, `draft_layers=1`); a regressão fica
em `flow_fase4_cli::project_delete_ordena_e_zera_banco`.

## 3. Índice teste → arquivo (caminhos relativos ao repo)

- `crates/ta-engine/src/grid.rs` — `grade_da_aoi_fixture_tem_118_celulas`,
  `fronteira_da_aoi_segue_intervalo_semiaberto`, `buraco_da_aoi_exclui_celulas`,
  `grade_grande_demais_e_rejeitada_sem_alocar`, `borda_parcial_mantida_quando_o_centro_esta_dentro`.
- `crates/ta-engine/src/rule.rs` — `prioridade_maior_vence_independente_da_ordem`,
  `avaliacao_e_deterministica_inclusive_com_ordem_trocada`,
  `medida_ausente_torna_a_comparacao_falsa_e_e_registrada`,
  `sintaxe_invalida_traz_a_posicao`, `and_tem_precedencia_sobre_or`.
- `crates/ta-engine/src/batch.rs` — `lote_e_deterministico`,
  `lote_vazio_devolve_vazio`, `ordem_de_entrada_e_preservada`.
- `crates/ta-cli/tests/flow_cli_integration.rs` — fase/AOI/grade/bloqueio.
- `crates/ta-cli/tests/flow_fase2_cli.rs` — classes/ruleset/regras.
- `crates/ta-cli/tests/flow_fase3_cli.rs` — medidas/task/process/log.
- `crates/ta-cli/tests/flow_fase4_cli.rs` — cenas/exchange/project delete.
- `crates/ta-cli/tests/flow_fase5_area_cli.rs` — área (texto/JSON/erros).
- `crates/ta-db/tests/pg_fase1_repos.rs` — fase, AOI, células, bloqueio.
- `crates/ta-db/tests/pg_fase2_repos.rs` — classes, rulesets, FKs.
- `crates/ta-db/tests/pg_fase3_queue.rs` — fila, retomada, F5 triggers.
- `crates/ta-db/tests/pg_fase4_scenes.rs` — cenas e proveniência.
- `crates/ta-db/tests/pg_fase5_area.rs` — área e ordem de grandeza.
- `crates/ta-db/tests/pg_grid_srid.rs` — grade métrica → células 4674.
- `crates/ta-db/tests/pg_viewport_contract.rs` — contrato da viewport.
- `crates/ta-gui/tests/map_view_contract.rs` — overlay/pick/legenda.
- `crates/ta-gui/tests/pg_gui_monitoring.rs` — GUI ponta a ponta vs CLI.
- `crates/ta-gis/tests/round_trip.rs` — round-trip por formato.

## 4. Lacunas e defeitos conhecidos (honestidade)

1. **Visualização sem display.** Não houve inspeção humana. `curupira-gis` sem
   `DISPLAY` falha com `WinitEventLoop(Os(... "neither WAYLAND_DISPLAY nor
   WAYLAND_SOCKET nor DISPLAY is set."))`. Estado ⚠️, não ✅.
2. **MVT não medido** neste host (PostGIS conda sem protobuf-c). O bench
   anuncia o cenário como pulado; nenhum número inventado.
3. **Escala 1M** não medida. Há dois limites: teto do motor `MAX_CELLS =
   5 000 000` (`ta-engine/src/grid.rs:64`) e teto de negócio default
   `projects.max_cells = 1 000 000`; nenhum é capacidade demonstrada.
4. **`project delete` — atribuição de `draft_layers`.** Corrigido na Fase 6:
   a ordem passou a apagar `draft_layers` **antes** de `cell_layers` (a FK
   `draft_layers.cell_layer_id` é `ON DELETE CASCADE`), então preview e
   relatório coincidem. Regressão em
   `flow_fase4_cli::project_delete_ordena_e_zera_banco` (preview == total,
   `draft_layers=1`).
5. **Cobertura abaixo de 85% em `ta-gui` (64,79%) e `ta-cli` (72,02%).** A
   meta de 85% do §8 é para `ta-engine` e `repo_*.rs`; a global (70%) é
   atingida.
6. **Regras sem predicado geoespacial.** O motor avalia medidas numéricas; não
   há regra sobre geometria/tempo.

## 5. Limites de afirmação

Não afirmamos: paridade visual, ergonomia, fidelidade de layout, exatidão
absoluta de área, processamento de imagem, desempenho em escala, nem
comportamento do produto de referência além do publicamente documentado. Ver
`docs/PARITY.md` §“O que NÃO podemos afirmar”.
