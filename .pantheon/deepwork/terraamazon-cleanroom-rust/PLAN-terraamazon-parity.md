# PLAN — Paridade do Fluxo de Monitoramento (TerraAmazon 7.3.2, clean-room)

- **Slug:** `terraamazon-cleanroom-rust`
- **Tipo:** plano aprovado pelo usuário (registro do MNEMOSYNE)
- **Aprovação:** 2026-10-18
- **Repositório:** `/home/jovyan/testes/terraamazon-cleanroom-rust` (publicado em https://github.com/ils15/curupira-gis)
- **Relação com os registros anteriores:** **não substitui** `PLAN.md` (Gate 1, v0.1.0→v0.2.2) nem as seções históricas de `STATUS.md`. É um **re-planejamento** que reusa o que já existe (workspace, licença dual, barreira clean-room, PostGIS/`sqlx`, `ta-cli`, `ta-gui`) e renumera as fases do **fluxo de monitoramento** de `0` a `6`.
- **Estado:** **Fase 0 concluída**; **Fase 1 implementada e revisada** (Themis, 2026-10-18) e **aguardando commit**. Fases 2–6 **pendentes**.

> **Proveniência / barreira clean-room.** Este documento é derivado **apenas** de documentação pública e especificação comportamental (ver `CONTRIBUTING.md`, linhas 16–29). **Nenhum** conteúdo derivado de engenharia reversa do produto legado está aqui — nem offsets, nem assembly/bytecode, nem pseudocódigo decompilado, nem schema/SQL extraído de binário, nem `hex dumps`, nem símbolos internos. As únicas menções ao material legado são de **proibição e remoção** (Decisão 1).

---

## 1. Objetivo

Tornar o **Curupira GIS** uma **réplica funcional clean-room** do **FLUXO DE MONITORAMENTO** do TerraAmazon 7.3.2.

A paridade perseguida é **comportamental, não de código**: o programa deve reproduzir o *que* o usuário observa fazer (projeto → fase → AOI → células → classes → regras → fila de processos → classificação → área → import/export → visualização), nunca *como* o produto legado implementa isso internamente.

## 2. Diagnóstico raiz (por que re-planejar)

Medido no repositório em 2026-10-18:

| Sintoma | Medida |
|---|---|
| GUI-first | `crates/ta-gui`: **12.270 linhas** `.rs` (src + tests) — a maior parte do produto |
| Domínio raso | `crates/ta-core`: **419 linhas** (tipos `Project`/`Layer`/`Feature`/`Style`) |
| Schema raso | **3 tabelas**: `projects`, `layers`, `features` (migrações `0001`–`0004`) |

Conclusão: a paridade **não** se obtém acrescentando telas sobre um domínio de 3 tabelas. Ela exige abordagem **domain-first**: modelar projeto/fase/AOI/células/classes/regras/fila como domínio testável e só então ligar UI, IO e persistência. Este plano é essa inversão.

## 3. Escopo

### 3.1 IN (paridade alvo)

- Projeto e **fase**
- **AOI** (área de interesse) e camadas AOI
- **Células** (grid) e **células bloqueadas**
- **Classes** e grupos de classes
- **Regras / rulesets**
- **Fila de processos** (tasks/processos e log por célula)
- **Classificação básica** (por regras)
- **Área** (cálculo)
- **Import/Export** (exchange de dados do fluxo)
- **Visualização** (AOI, células, classes no canvas)

### 3.2 OUT (explicitamente fora)

Raster pesado, DTM, gráficos, layout de impressão, servidor WMS/WCS, geocoding, plugin manager real, multi-usuário, bookmarks, image servers.

### 3.3 Fora por barreira (não por escopo)

Qualquer insumo derivado do binário/instalador legado ou de engenharia reversa (ver Decisão 1).

## 4. Arquitetura alvo (crates)

| Crate | Papel neste plano |
|---|---|
| `ta-core` | Tipos de domínio compartilhados (projeto, fase, AOI, célula, classe, regra, processo, cena) |
| **`ta-engine` (NOVO)** | **Motor de domínio puro**: depende **apenas** de `ta-core` + `geo-types`. Sem IO, sem DB, sem GUI. Onde vive a paridade comportamental testável |
| `ta-db` | Persistência `sqlx`/PostGIS, repositórios e migrações |
| `ta-gis` | IO geoespacial, import/export e cálculo auxiliar |
| `ta-gui` | Visualização e fluxo de trabalho (consome o motor; não contém regra de domínio) |
| `ta-cli` | Automação/verificação do fluxo (doctor, seed, comandos do fluxo) |

**Decisão 3** fixa: o motor é o crate **novo e puro** `ta-engine` (só `ta-core` + `geo-types`).

## 5. DAG de dependências

```
Fase 0 ──► Fase 1 ──► Fase 2 ──► Fase 3 ──► Fase 5 ──► Fase 6
                       │
                       └──► Fase 4  (paralelizável após a Fase 1)
```

- Cadeia principal: **0 → 1 → 2 → 3 → 5 → 6**
- **Fase 4 (scenes/exchange)** independente da 2 e da 3: pode rodar **em paralelo após a Fase 1**.

## 6. Fases

> **Granularidade.** Os itens de escopo, o DAG e as migrações abaixo estão fixados na aprovação. O detalhamento interno de cada fase (ordem de tarefas, nomes de comandos, campos exatos) é **derivado** do escopo aprovado e deve ser **confirmado no gate da própria fase** — não é inventado aqui como se estivesse aprovado.

### Fase 0 — Usabilidade ponta a ponta — **CONCLUÍDA**

Tornar o projeto utilizável de ponta a ponta **antes** de aprofundar o domínio.

**Entregáveis**
- `scripts/dev-up.sh`: provisionamento do ambiente **autodetectando docker**; se docker não existir, **fallback conda** via `scripts/test-pg.sh` (Decisão 5).
- **doctor**: diagnóstico de ambiente (toolchain Rust, GDAL/PROJ, PostGIS, docker/conda) com saída acionável.
- **seed-demo**: dataset de demonstração determinístico para exercitar o fluxo.
- **baseline**: medição versionada de partida (LOC, contagem de testes, cobertura) para comparar progresso.
- **CI não-gating**: pipeline informativo que **não bloqueia** merge enquanto a paridade é construída.
- **docs**: documentação de setup/uso do fluxo.

**Arquivos/crates afetados:** `scripts/` (`dev-up.sh`, `test-pg.sh`, seed), `.github/workflows/`, `README.md`/`INSTALL.md` (ou `docs/`), `ta-cli` (doctor/seed), eventualmente `ta-db` (dados de seed). *Sem alteração de schema.*

**Critérios de aceite**
- Um dev novo sobe o ambiente com **um comando**, em host com docker **e** em host só-conda.
- `doctor` reporta o estado real do ambiente e o que falta instalar.
- `seed-demo` popula o banco de forma **determinística** (reproduzível).
- Baseline registrado e comparável.
- CI roda como informativo **sem** bloquear.
- Docs descrevem o fluxo ponta a ponta acima.

**Riscos**
- Docker ausente no host (mitigado pelo fallback conda — Decisão 5).
- CI não-gating pode **mascarar regressões** (mitigação: gate local `fmt`/`clippy`/`test` antes do commit manual).
- Deriva de ambiente (versões de GDAL/PostGIS) — pinar versões.

### Fase 1 — Projeto / Fase / AOI / Células bloqueadas

Base **domain-first**: o esqueleto do fluxo de monitoramento.

**Entregáveis**
- Modelo de domínio: **projeto**, **fase**, **camadas AOI**, **grid de células**, **células bloqueadas**.
- Persistência e repositórios correspondentes.
- Comandos de CLI para criar/consultar o fluxo.
- **`ta-engine` criado** (crate puro) com grid/resolução/área.

**Migrações:** `0005_phases`, `0006_aoi_cells`, `0007_grid_srid` (retificação da Decisão 4 em 2026-10-18).
**Tabelas (nomes próprios, Decisão 2):** `phases`, `area_settings`, `app_meta`; `aoi_layers`, `cell_layers`, `cells`, `blocked_cells`.
**Crates:** `ta-core` (tipos), **`ta-engine` (novo)**, `ta-db` (repos + migrações), `ta-cli`.

**Critérios de aceite**
- Criar projeto e fase; definir AOI; **gerar grid de células**; **bloquear** células.
- Geometria persistida em **EPSG:4674**; a grade é configurável em **qualquer EPSG** (resolução/origem nas unidades do SRS escolhido) e a **área** usa o SRS de `area_settings.srs_id` (default EPSG:5880) — Decisão 4 retificada em 2026-10-18.
- Motor puro coberto por testes unitários (sem DB); persistência coberta por integração PG.

**Riscos**
- Escolha do SRS da grade (graus vs metros) — critério objetivo agora é do usuário (`--srid`), com teste de paridade motor↔PostGIS.
- Performance do grid em AOI grande (não otimizar antes de medir).

### Fase 2 — Classes e regras (rules/rulesets)

**Entregáveis**
- Classes e **grupos de classes**; **camadas de rascunho** de classificação.
- **Regras** e **rulesets** (versionados).
- Avaliação de regra sobre célula **no motor puro** (testável sem DB/GUI).

**Migrações:** `0009_classes`, `0010_rules`.
**Tabelas:** `classes`, `class_groups`, `draft_layers`; `rules`, `rulesets`.
**Crates:** `ta-core`, `ta-engine` (avaliação), `ta-db`, `ta-cli`.

**Critérios de aceite**
- CRUD de classes/grupos e de rules/rulesets.
- Motor avalia regra → decisão de classe de forma **pura e determinística** (testes unitários).

**Riscos**
- Escopo de "regra" pode inflar (YAGNI): manter conjunto mínimo que a paridade comportamental exige.

### Fase 3 — Fila de processos + classificação básica

**Entregáveis**
- **Fila de processos**: `tasks`, `processes` e **log por célula** (`cell_log`).
- **Classificação básica** executada pelo motor, com estado retomável.
- Comandos de CLI para enfileirar/acompanhar.

**Migração:** `0011_queue`.
**Tabelas:** `tasks`, `processes`, `cell_log`.
**Crates:** `ta-engine` (execução), `ta-db`, `ta-cli`.

**Critérios de aceite**
- Enfileirar processo por célula; executar; registrar log por célula; refletir estado (pendente/em execução/concluído/erro).

**Riscos**
- Concorrência/retomada da fila — manter modelo simples e observável nesta fase.

### Fase 4 — Scenes e exchange — **paralelizável após a Fase 1**

**Entregáveis**
- Registro de **scenes** e vínculo com o projeto (`project_scenes`).
- **Import/Export** de exchange do fluxo.

**Migração:** `0012_scenes`.
**Tabelas:** `scenes`, `project_scenes`.
**Crates:** `ta-core`, `ta-db`, `ta-gis` (IO), `ta-cli`.

**Critérios de aceite**
- Cadastrar cena e vinculá-la ao projeto; importar/exportar o exchange com round-trip verificado.

**Riscos**
- Amplitude do formato de exchange (manter mínimo e documentado).

### Fase 5 — Área, import/export e visualização

Integração do fluxo completo na interface e consolidação de **área**.

**Entregáveis**
- **Cálculo de área** consolidado (em **EPSG:5880** ou `geography`, Decisão 4).
- **Import/Export** integrados ao fluxo.
- **Visualização** de AOI, células (bloqueadas/incluídas) e classes no canvas.

**Crates:** `ta-gis`, `ta-db`, `ta-gui`, `ta-cli`.
**Depende de:** Fases 3 e 4.

**Critérios de aceite**
- Fluxo ponta a ponta executável pela GUI **e** pela CLI, com resultados consistentes.
- Área calculada de forma verificável (teste de ordem de magnitude em 5880 vs 4674).

**Riscos**
- `ta-gui` crescer como dono de regra de domínio — a regra **deve** ficar no `ta-engine`.

### Fase 6 — Paridade comportamental e fechamento

**Entregáveis**
- **Matriz de paridade comportamental** contra **documentação pública** (checklist observável, sem material legado).
- Documentação final do fluxo.
- Fechamento das metas de qualidade (§8).

**Critérios de aceite**
- Cada item IN (§3.1) com evidência de comportamento observável documentada.
- Metas do §8 atingidas ou desvios registrados honestamente.

**Riscos**
- Alegar "paridade" além da evidência — a matriz deve distinguir **verificado** de **não verificado**.

## 7. Migrações planejadas

Sequência (continua `0001`–`0004` já existentes):

| Migração | Fase | Conteúdo previsto (nomes próprios) |
|---|---|---|
| `0005_phases` | 1 | `phases`, `area_settings`, `app_meta` |
| `0006_aoi_cells` | 1 | `aoi_layers`, `cell_layers`, `cells`, `blocked_cells` |
| `0007_grid_srid` | 1 | `cell_layers.srid` (SRS da grade; retificação da Decisão 4, 2026-10-18) |
| `0008_cell_validity` | 1 | trigger `cells_geom_validity` (rejeita célula inválida; mesmo gate de `features`) |
| `0009_classes` | 2 | `classes`, `class_groups`, `draft_layers` |
| `0010_rules` | 2 | `rules`, `rulesets` |
| `0011_queue` | 3 | `tasks`, `processes`, `cell_log` |
| `0012_scenes` | 4 | `scenes`, `project_scenes` |

Todas usam **nomes próprios, sem prefixo legado** (Decisão 2). Geometria em **EPSG:4674**; a grade é configurável em **qualquer EPSG** (resolução/origem nas unidades do SRS escolhido) e a **área** usa o SRS de `area_settings.srs_id` (Decisão 4 retificada em 2026-10-18).

**Backfill da 0007 (intencional):** em bancos que aplicaram a `0006` da
iteração anterior (antes de `cell_layers.srid` existir), a `0007` adiciona a
coluna com `DEFAULT 4674` e faz backfill de todas as linhas para 4674. É o
valor correto para grades antigas (todas eram em graus) e bancos de dev podem
ser recriados; não há migração de dados a fazer.

**Semântica de borda do recorte (para quem reimplementar em SQL):** o
`generate_grid` inclui a célula pelo centro e trata o envelope como
**semiaberto** `[x_min, x_max) × [y_min, y_max)` — aresta de máximo fica fora.
`ST_Contains`/`ST_Covers` do PostGIS **não** são equivalentes nessa fronteira
(ambos cobrem o limite), então o recorte em SQL deve usar comparação
semiaberta (`>=` no mínimo, `<` no máximo), nunca essas funções.

## 8. Metas de qualidade

- **Cobertura:** **70% global**; **85%** em **`ta-engine`** e nos **repositórios de `ta-db`**.
- **Zero `unwrap`/`expect`** em **libs não-teste**.

## 9. Decisões aprovadas (registradas para não serem relitigadas)

| # | Decisão |
|---|---|
| **1 (a)** | A paridade é validada **apenas** por **documentação pública + observação comportamental**. É **PROIBIDO** usar engenharia reversa do produto legado. Por política clean-room (`CONTRIBUTING.md`), **todo** o material proibido foi **removido do disco em 2026-10-18** por decisão do usuário. Nenhum insumo derivado de engenharia reversa do produto legado pode entrar neste repositório. |
| **2 (sim)** | **Nomes de tabela próprios**, sem prefixo legado: `phases`, `aoi_layers`, `cell_layers`, `cells`, `blocked_cells`, `classes`, `class_groups`, `rules`, `rulesets`, `tasks`, `processes`, `cell_log`, `draft_layers`, `scenes`, `project_scenes`, `area_settings`, `app_meta`. |
| **3** | **Motor de domínio em crate NOVO e puro `ta-engine`** — depende **apenas** de `ta-core` + `geo-types`; **sem IO, sem DB, sem GUI**. |
| **4** | Decisão 4 (retificada pelo usuário em 2026-10-18): a grade é configurável em QUALQUER EPSG — resolução e origem nas unidades do SRS escolhido; armazenamento canônico das células em EPSG:4674; cálculo de área no SRS de `area_settings.srs_id` (default EPSG:5880). Não há escolha excludente entre graus e metros. |
| **5** | Provisionamento na **Fase 0** via `scripts/dev-up.sh` **autodetectando docker**, com **fallback conda** (`scripts/test-pg.sh`). |

**Diagnóstico que fundamenta o plano:** projeto GUI-first (**12.270** linhas de UI) sobre domínio raso (**3 tabelas**: `projects`/`layers`/`features`; `ta-core` com **419** linhas) ⇒ a paridade **exige** abordagem **domain-first**.

## 10. Registro de não-progresso (honestidade)

- **Fase 0**: concluída (2026-10-17/18).
- **Fase 1**: **implementada e revisada** (Themis: aprovada com ressalvas em
  2026-10-18); os achados de fechamento foram aplicados e a fase **aguarda
  commit** (migrações `0005`–`0008`, CLI `phase`/`aoi`/`grid`/`cell`, seed).
- Fases **2, 3, 4, 5 e 6**: **não iniciadas**.
- Dívidas de teste adiadas para o hardening da Fase 6: dedup de helpers de
  teste (n7), n8 e n13.
- Nada foi commitado por este registro (o usuário faz commits manualmente).
- Nenhum ADR foi criado (não solicitado).

## 11. Riscos transversais

1. **Contaminação clean-room** — mitigada pela barreira em `CONTRIBUTING.md` + deny-list; qualquer material derivado do legado é rejeição automática de PR.
2. **Alegar paridade além da evidência** — mitigado pela matriz de paridade da Fase 6 (verificado ≠ não verificado).
3. **UI absorver regra de domínio** — mitigado pela Decisão 3 (`ta-engine` puro).
4. **Deriva de versões** (GDAL/PostGIS/rust) — pinar e verificar no `doctor`.
5. **Escopo inflando além do IN** (§3.1) — itens OUT (§3.2) permanecem fora.
6. **FK para `spatial_ref_sys` exige privilégio** — a `0007` cria
   `cell_layers.srid → spatial_ref_sys(srid)`. Se o PostGIS foi criado por
   outro role, a migração falha com `permission denied` e é preciso
   `GRANT REFERENCES ON spatial_ref_sys TO <app_role>;` (ou criar a extensão
   com o role da aplicação) — ver `INSTALL.md`.
