# Curupira GIS

[![ci](https://github.com/ils15/curupira-gis/actions/workflows/ci.yml/badge.svg)](https://github.com/ils15/curupira-gis/actions/workflows/ci.yml)
[![release](https://github.com/ils15/curupira-gis/actions/workflows/release.yml/badge.svg)](https://github.com/ils15/curupira-gis/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE-MIT)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE-APACHE)

> **Clean-room desktop GIS for Amazon forest monitoring.**
> SIG desktop clean-room para monitoramento florestal da Amazônia.

## Visão geral

Curupira GIS é um SIG desktop em Rust que reproduz o **fluxo de monitoramento**
do TerraAmazon 7.3.2 de forma **independente (clean-room)**. O objetivo não é
copiar código: é reproduzir o que o usuário observa fazer —

**projeto → fase → AOI → grade de células → classes → regras (ruleset) →
fila de processos → classificação → área → import/export → visualização.**

O produto é útil para quem precisa de um fluxo de monitoramento reprodutível e
verificável: decisões de classe são tomadas por regras determinísticas sobre
medidas observadas, tudo fica persistido em PostgreSQL/PostGIS e o mesmo fluxo
roda pela **CLI** (automação) e pela **GUI** (operação). Nada de processamento
de imagem embutido: o app recebe medidas (índices, contagens) e aplica regras.

A matriz de paridade — item a item, com evidência e limitações — está em
**[`docs/PARITY.md`](docs/PARITY.md)**.

## Arquitetura

| Crate | Papel (uma linha) |
|---|---|
| `ta-core` | Tipos de domínio puros: projeto, fase, AOI, célula, classe, regra, cena; WKB/EWKB; erros. Sem IO/DB/GUI. |
| `ta-engine` | **Motor puro** da paridade: grade de células e parser/avaliador de regras. Depende só de `ta-core` + `geo-types`. |
| `ta-db` | Persistência `sqlx`/PostGIS, repositórios async e migrações embutidas. |
| `ta-flow` | Executor do fluxo: fila de processos, classificação em lote, área, fase/AOI/grade/ruleset/medidas. Usado por CLI e GUI. |
| `ta-gis` | IO geoespacial: leitura/escrita vetorial e raster, normalização para 4674, exchange, COG (GDAL opcional). |
| `ta-gui` | Desktop `egui/eframe` (binário `curupira-gis`): canvas, painéis e a janela **Monitoramento**. Orquestra; não contém regra de domínio. |
| `ta-cli` | Automação e verificação (binário `ta-cli`): `doctor`, `migrate`, `seed-demo`, import/export, fluxo completo. |

Dependências (setas = “depende de”):

```
ta-core                tipos puros (sem IO/DB/GUI)
├── ta-engine          → ta-core + geo-types (motor puro)
├── ta-db              → ta-core (persistência; reexporta sqlx)
│   └── ta-flow        → ta-core + ta-db + ta-engine (fila, área, preparo)
├── ta-gis             → ta-core (+ ta-db opcional, feature `db`)
├── ta-gui             → ta-core + ta-db + ta-flow + ta-gis (desktop)
└── ta-cli             → todos os anteriores (automação/verificação)
```

Regra estrutural: **domínio no `ta-engine`**, persistência no `ta-db`,
orquestração no `ta-flow`; `ta-gui`/`ta-cli` apenas dirigem. É isso que impede
a UI de reimplementar regra de negócio (e o que torna a paridade testável sem
display).

## Quickstart

Pré-requisitos: **Rust** (stable, `rust-toolchain.toml`) e **GDAL** do sistema
(`sudo apt install libgdal-dev`). Banco: **docker** *ou* **conda** — o script
escolhe. Detalhes e troubleshooting em **[INSTALL.md](INSTALL.md)**.

```sh
bash scripts/dev-up.sh          # banco + schema + projeto `demo` (idempotente)
bash scripts/dev-up.sh --gui    # o mesmo e abre o desktop (precisa de display)
```

O `dev-up.sh`:

1. garante PostgreSQL 16 + PostGIS 3.4 (docker, ou cluster efêmero conda);
2. roda `ta-cli doctor` (informativo);
3. aplica as **14 migrações** (`ta-cli migrate`, idempotente);
4. roda `ta-cli doctor` de novo — **reprova o script** se algo falhar;
5. semeia o projeto `demo` (`ta-cli seed-demo`).

Testes de integração com PostGIS são **gated** por `TA_TEST_PG=1` e não quebram
a suíte sem banco:

```sh
bash scripts/test-pg.sh        # sobe o cluster, roda a suíte gated e derruba
```

DSN em ordem de precedência: `--dsn` > `TA_PG_DSN` >
`postgres://ta:ta@localhost:5432/ta`.

## Fluxo completo pela CLI

O exemplo abaixo é o fluxo real, executado e conferido (números de um dataset
sintético; os seus variam). IDs entre `<…>` saem da própria saída dos comandos.

```sh
# 0) ambiente e projeto — binários do próprio build (sem depender do PATH)
export PATH="$PWD/target/debug:$PATH"
bash scripts/dev-up.sh
ta-cli import-vector --input testdata/monitoring/aoi.geojson \
    --project minha-area --layer-name aoi_area
ta-cli list-layers --project minha-area        # UUID da camada AOI

# 1) fase (agrupa AOI/grade; `--cell-blocker` marca fase bloqueadora)
ta-cli phase create --project minha-area --name "2026-10" --sort-order 1
ta-cli phase list   --project minha-area

# 2) AOI = camada poligonal existente eleita área de interesse
ta-cli aoi create --project minha-area --layer <UUID camada>

# 3) grade de células (resolução nas unidades do SRS escolhido)
#    graus em 4674: 0.005°; metros em 5880: 500
ta-cli grid create --aoi-layer <UUID aoi> --resolution 0.005 --name grade-4674
# ta-cli grid create --aoi-layer <UUID aoi> --resolution 500 --srid 5880 --name grade-5880
ta-cli grid info --cell-layer <UUID grade>

# 4) classes e grupos
ta-cli class group create --project minha-area --name cobertura
ta-cli class create --project minha-area --name floresta \
    --color "#0A7D2C" --group cobertura
ta-cli class create --project minha-area --name desmatamento \
    --color "#C62828" --group cobertura

# 5) ruleset versionado + regras + ativação (default do `task create`)
ta-cli ruleset create --project minha-area --name regras
ta-cli rule add --ruleset <UUID ruleset> --class floresta --name f \
    --priority 10 --when "ndvi >= 0.6"
ta-cli rule add --ruleset <UUID ruleset> --class desmatamento --name d \
    --priority 5 --when "ndvi < 0.3 and nbr < 0.2"
ta-cli ruleset activate --project minha-area --name regras
ta-cli rule eval --ruleset <UUID ruleset> --obs "ndvi=0.7,nbr=0.05"

# 6) medidas observadas (CSV LONG `cell_id,measure,value[,scene]`)
ta-cli measure import-csv --cell-layer <UUID grade> \
    --input testdata/monitoring/medidas.csv
#    ou pontual: ta-cli measure set --cell-layer <UUID> --cell-id 1_1 \
#                --values "ndvi=0.72,nbr=0.05"

# 7) fila de processos + classificação (retomável, lote transacional)
ta-cli task create --project minha-area --cell-layer <UUID grade> classify
ta-cli task run  <UUID task>          # --limit N processa no máximo N células
ta-cli task show <UUID task>
ta-cli process list --task <UUID task> --status done
ta-cli cell log --cell-layer <UUID grade> --cell-id 1_1

# 8) rascunho e área (SRS métrico de area_settings.srid_id; default 5880)
ta-cli draft list --project minha-area
ta-cli area report --cell-layer <UUID grade> \
    --draft "rascunho regras" --json

# 9) bloqueio de célula
ta-cli cell block --cell-layer <UUID grade> --cell-id 0_1 --reason "nuvem"
ta-cli cell list-blocked --cell-layer <UUID grade>

# 10) export/import do exchange (projeto NOVO; round-trip verificado)
ta-cli exchange export --project minha-area --out /tmp/exchange.json
ta-cli exchange import --in /tmp/exchange.json --into minha-area-copia
```

Observações de operação:

- `task run` processa **todas as células elegíveis**; grava decisão apenas nas
  que têm medidas **e** regra que casa. Células bloqueadas ficam fora do
  enfileiramento (`... (bloqueadas fora)`).
- `task cancel` é cooperativo (para entre lotes); `task retry` repõe apenas
  `error → pending`.
- `class delete`/`ruleset delete`/`draft delete` bloqueiam com **contagem** e
  exigem `--force`/`--yes` (caminho de saída explícito, sem cascata silenciosa).
- `project delete --project <nome> --yes` apaga na ordem correta, em transação
  única, com relatório por tabela; sem `--yes` mostra o resumo e recusa.

## GUI — o mesmo fluxo sem CLI

Abra com `bash scripts/dev-up.sh --gui` (ou `cargo run -p ta-gui` com
`TA_DATABASE_URL` preenchida). **Conectar** → **Ctrl+O** → projeto.

A janela **Ferramentas → Monitoramento** executa o fluxo completo:

| Aba/ação | O que faz |
|---|---|
| **Fase** | cria fase (nome/descrição) e lista as fases. |
| **AOI** | escolhe uma camada poligonal e a coluna de id → “Vincular AOI”. |
| **Grade** | escolhe a AOI, nome, resolução e EPSG → “Gerar grade”. |
| **Ruleset** | lista versões e “Ativar selecionado” (default do “Classificar”). |
| **Medidas** | importa CSV LONG `cell_id,measure,value[,scene]`; inválidas aparecem com motivo; reimportar é idempotente. |
| **Classificar** | cria e roda a task no worker (UI responsiva); “Cancelar” pede parada cooperativa. |
| **Células** | “Selecionar células no mapa” liga o pick; clique bloqueia/desbloqueia. A viewport consulta por bbox com margem e `LIMIT` de 5.000 (aviso quando trunca). |
| **Área** | relatório consolidado (total/bloqueada/disponível e por classe), o mesmo contrato da CLI. |
| **Exchange** | export/import via binário `ta-cli` (mesma validação; DSN por ambiente, nunca na linha de comando). |

O canvas desenha AOI, células coloridas por classe (bloqueio vence a classe;
sem decisão = cinza) e a legenda derivada das células visíveis. A GUI
**orquestra** `ta-flow`/`ta-db` — não reimplementa regra de domínio.

## Decisões de projeto

| # | Decisão |
|---|---|
| **2** | **Nomes próprios**, sem prefixo legado: `phases`, `aoi_layers`, `cell_layers`, `cells`, `blocked_cells`, `classes`, `class_groups`, `rules`, `rulesets`, `tasks`, `processes`, `cell_log`, `draft_layers`, `scenes`, `project_scenes`, `area_settings`, `app_meta`. |
| **3** | **Motor puro `ta-engine`** (só `ta-core` + `geo-types`): sem IO, DB ou GUI. A paridade comportamental é testável sem infraestrutura. |
| **4 (retificada)** | **Multi-EPSG**: a grade é configurável em **qualquer EPSG** — `--resolution`/`--origin-*` nas unidades do SRS escolhido (graus em 4674, metros em 5880). As células são **sempre persistidas em EPSG:4674** (armazenamento canônico). A **área** usa o SRS métrico de `area_settings.srs_id` (default **EPSG:5880**); SRS geográfico é recusado. |
| **5** | Provisionamento em um comando (`dev-up.sh`) **autodetectando docker**, com **fallback conda** (`test-pg.sh`). |

Por que **4674 canônico**: SIRGAS 2000 é o datum legal da cartografia
brasileira; armazenar tudo nele evita ambiguidade entre camadas e mantém
`ST_SRID(geom)=4674` como CHECK no banco. Por que **área em 5880**: calcular
área em graus não tem significado métrico; a Policônica SIRGAS 2000 dá metros
na região amazônica.

## Qualidade

| Check | Resultado (2026-10-19) |
|---|---|
| `cargo fmt --all -- --check` | ✅ |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✅ |
| `cargo test --workspace` | ✅ **369 testes / 35 alvos** (21 integração + 7 unit + 7 doc-tests) |
| `TA_TEST_PG=1 … cargo test --workspace` | ✅ 369/369, 0 ignored; **3 testes de MVT auto-pulam** em hosts sem protobuf-c (a prova real de MVT é o job `linux-pg` do CI) |
| `cargo deny check` | ✅ `advisories/bans/licenses/sources ok` |
| `bash scripts/dev-up.sh` (2×) | ✅ idempotente, doctor exit 0 |

Cobertura (Fase 6a, Postgres real, `cargo llvm-cov`): **global 73,48%**;
`ta-engine` **98,53%**; `ta-db` **91,97%** (`repo_*.rs` **92,61%**). Metas do
plano: 70% global e 85% em `ta-engine`/repositórios — atingidas. `ta-gui`
(64,79%) e `ta-cli` (72,02%) **não** têm meta de 85% (ver limitações).

Bench (`benches/BASELINE.md`, banco limpo, mediana de 3 execuções):
`bulk_insert/10k` **511,5 ms**; `bbox_query` **12,57 ms**; `list_with_ids`
**28,21 ms**; **MVT não medido** neste host (PostGIS sem protobuf-c). Render da
GUI headless: p95 pior **5,91 ms**.

## Limitações conhecidas

- **Sem verificação visual humana**: o host de referência não tem display; a
  visualização é verificada por **contrato headless** (cores, pick, viewport).
  GPU não é medida.
- **Sem processamento de imagem**: classificação é por **regras sobre medidas
  importadas**; não há cálculo de índices espectrais nem leitura de pixels.
- **Escala não medida**: o teto do **motor** é `MAX_CELLS = 5 000 000`
  (`ta-engine`), e o teto de **negócio** default é `projects.max_cells =
  1 000 000`; nenhum dos dois é capacidade demonstrada. O bench cobre 10 000
  feições.
- **MVT sem número** neste host (falta protobuf-c no PostGIS conda).
- **Área projetada**, não geodésica (ordem de grandeza testada).
- **Exchange é formato próprio** (`curupira-monitoring-exchange v1`), sem merge
  e sem raster.
- **`project delete` corrigido na Fase 6**: o relatório contava
  `draft_layers=0` (CASCADE de `cell_layers`) e o total ficava 1 abaixo do
  preview; a ordem foi corrigida (rascunhos antes das grades) e há regressão
  em `flow_fase4_cli` comparando preview e delete.
- **Dívidas de teste**: dedup de helpers (n7/n8/n13) ainda aberto.

## Clean-room

Este projeto é uma **reimplementação independente**. É terminantemente proibido
(ver `CONTRIBUTING.md`):

1. inspecionar binário/instalador/pacote legado ou conteúdo derivado;
2. descompilar, desassemblar, tracejar ou fazer engenharia reversa;
3. copiar código, pseudocódigo, recursos, ícones, strings ou layouts.

A única fonte de especificação é o **comportamento público** (documentação de
usuário, manuais, tutoriais, observação do usuário final). A matriz
`docs/PARITY.md` cita apenas o plano aprovado, evidência do nosso sistema e,
quando necessário, documentação pública — nunca material de engenharia
reversa. Aviso legal em [`NOTICE.md`](NOTICE.md).

## Instalação e releases

Do código-fonte: **[INSTALL.md](INSTALL.md)**. Binários de release (Linux
`tar.gz`/`.deb`, Windows zip/instalador com GDAL embutida, macOS arm64 não
assinado) em [Releases](https://github.com/ils15/curupira-gis/releases) —
publicados como **draft** até revisão humana.

Licença dual **MIT OR Apache-2.0** (`LICENSE-MIT`, `LICENSE-APACHE`).
Checkpoints de engenharia em `.pantheon/deepwork/terraamazon-cleanroom-rust/`.
