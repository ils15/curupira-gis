# Bench baseline — `ta-db` (Fase 0)

> Contrato: Fase 0 — "baseline publicado com números do host". Harness:
> `crates/ta-db/benches/ta_db_baseline.rs` (`cargo bench -p ta-db`), dependência
> de desenvolvimento `criterion` 0.5.1 (runner textual, sem `plotters`/`rayon`).
>
> Dataset: 10 000 polígonos pequenos + 3 atributos (`id`, `nome`, `classe`),
> EPSG:4674 — **o mesmo gerador do teste gated** `tests/pg_integration.rs`
> (`bulk_insert_10k_em_tempo_documentado`), para os números serem comparáveis.

## Resultado executado (2026-10-18, host do projeto)

Ambiente: Ubuntu 24.04 (kernel 5.15), Intel Xeon Gold 5120 (52 vCPU, host
compartilhado), 472 GiB RAM, rustc 1.99.1 stable, perfil `bench`
(opt-level 3, `--release`). Banco: **PostgreSQL 16.10 + PostGIS 3.4.3** em
cluster efêmero conda-forge (`scripts/test-pg.sh --no-tests`, porta 55432).
Criterion: `sample_size = 10`, warm-up 1 s, medição 3 s (o cenário de
`bulk_insert` estendeu para ~5,1 s para completar as 10 amostras).

| cenário | mean | [IC 95% do criterion — 10 amostras, 1 execução] | o que mede |
|---|---|---|---|
| `bulk_insert/10k_poligonos` | **500,9 ms** | [435,1 ms – 578,7 ms] | COPY FROM STDIN de 10k polígonos + atributos (fallback batched INSERT) |
| `bbox_query/janela_viewport` | **12,13 ms** | [12,03 ms – 12,24 ms] | `&&` + GIST devolvendo ~4 000 feições (10°×4°) com geometria e attrs |
| `list_with_ids/10k` | **26,07 ms** | [25,83 ms – 26,43 ms] | leitura completa (id de linha + EWKB + attrs) dos 10k |
| `mvt_tile/z12_viewport` | **não medido** | — | `ST_AsMVT` — ver limitação abaixo |

> **Como ler os dois intervalos deste arquivo.** O intervalo da tabela acima
> é o **IC 95% do criterion** dentro de **uma execução** de 10 amostras
> (2026-10-18) — o que o runner imprime. O da re-execução de fechamento
> (Fase 6a, tabela mais abaixo) é a **faixa das 3 execuções**, isto é,
> **mínimo–máximo entre execuções**, não um intervalo de confiança. As duas
> grandezas não são comparáveis entre si; a comparação da Fase 6a usa a
> coluna "baseline (2026-10-18)" (as médias), não os intervalos.

Leitura dos números:

- **Insert** (~50 µs/feature): custo dominado pelo lado do servidor —
  trigger `features_geom_validity` (`ST_IsValid` por linha), EWKB textual no
  COPY e manutenção do índice GIST. Não é gargalo de cliente: é o preço
  documentado da política "validar na inserção" (0002_features.sql).
- **Query de viewport** (~12 ms para ~4 000 feições): ~3 µs/feição, incluindo
  decode EWKB no cliente — é este o caminho do render da GUI.
- **`list_with_ids` dos 10k** (~26 ms): ~2,6 µs/feição; alimenta a tabela
  atributiva e o undo.
- A variância do `bulk_insert` (±14% no IC) reflete o host compartilhado
  (52 vCPU com outros processos): rodar 3× e comparar medianas é mais
  confiável que uma execução isolada.

Método do `bulk_insert` (importante para reproducibilidade): a limpeza da
camada (`DELETE FROM features WHERE layer_id = …`) roda no **setup** de cada
amostra, portanto **fora** da medição — sem isso a tabela cresceria a cada
iteração e o número derivaria para cima. O tempo medido é o do
`bulk_insert` sobre uma camada com ~10k linhas pré-existentes.

## Limitação honesta: `mvt_tile` não foi medido neste host

O build conda-forge do PostGIS **não traz protobuf-c**, então `ST_AsMVT`
existe mas falha em runtime; o harness detecta via
`ta_db::spatial::mvt_available` e **anuncia o cenário como pulado** em vez de
falhar. Números de MVT exigem um servidor com protobuf-c — a imagem
`postgis/postgis:16-3.4` (usada no job `linux-pg` do CI, onde o step de
baseline é não-gating) tem. Este host não tem docker/podman, logo o cenário
fica **pendente de medição** até a primeira run de CI com o service PostGIS
(ou um host com docker). Não há número inventado aqui.

## Como reproduzir

```sh
# cluster PG16 + PostGIS 3.4 efêmero (mesmo pins do docker-compose.yml)
bash scripts/test-pg.sh --no-tests          # imprime o DSN e o comando de parada

# banco LIMPO por rodada: o harness roda as migrações, mas não limpa um
# database reaproveitado. Rodar sobre o banco de trabalho acumulado infla o
# `bbox_query` (índice GIST global maior) — ver "Efeito de banco sujo" abaixo.
psql "postgres://ta:ta@127.0.0.1:55432/postgres" \
    -c "CREATE DATABASE ta_bench"

# baseline (o output traz mean/IC por cenário)
TA_TEST_PG=1 TA_PG_DSN=postgres://ta:ta@127.0.0.1:55432/ta_bench \
    cargo bench -p ta-db --bench ta_db_baseline
```

Sem `TA_TEST_PG=1` o harness imprime o motivo e sai 0 — `cargo bench` nunca
quebra por ausência de banco. No CI o step roda com `continue-on-error: true`
(não-gating), no job que já tem o service PostGIS.

## Re-execução de fechamento (Fase 6a) — 2026-10-19

Ambiente: o MESMO host da baseline acima (Linux 5.15, Intel Xeon Gold 5120,
52 vCPU, 472 GiB, rustc 1.99.1 stable, perfil `bench`), commit
`75d921d`, banco **limpo por execução** (`CREATE DATABASE ta_bench` novo →
`migrate` → bench; PostgreSQL 16.10 + PostGIS 3.4.3 conda-forge, porta
55432). O `ta-db` foi rodado **3×** (uma execução = 10 amostras, warm-up 1 s);
a tabela abaixo usa a **mediana das médias** das 3 execuções — o host é
compartilhado e o `bulk_insert` varia ±20% entre execuções.

| cenário | mediana das médias | faixa das 3 execuções | baseline (2026-10-18) | Δ |
|---|---|---|---|---|
| `bulk_insert/10k_poligonos` | **511,5 ms** | 452,5 – 584,6 ms | 500,9 ms | **+2,1%** (ruído) |
| `bbox_query/janela_viewport` | **12,57 ms** | 12,29 – 12,97 ms | 12,13 ms | **+3,6%** |
| `list_with_ids/10k` | **28,21 ms** | 25,53 – 30,26 ms | 26,07 ms | **+8,2%** |
| `mvt_tile/z12_viewport` | **não medido** | — | não medido | — |

Leitura honesta:

- **Não há regressão conclusiva.** As três faixas se sobrepõem à baseline; o
  `list_with_ids` (+8,2%) é o maior desvio e cabe na variância de ±15%
  documentada para este host compartilhado. Nenhuma mudança de código de
  `ta-db` nesta fase alterou os caminhos medidos (só o teste gated mudou).
- **MVT continua pulado** pelo mesmo motivo da baseline: o build conda-forge
  do PostGIS não traz protobuf-c. Nenhum número inventado; a medição real de
  MVT segue pendente do job `linux-pg` do CI (imagem com protobuf-c).
- **Efeito de banco sujo (metodológico):** a primeira execução desta fase foi
  feita no banco de trabalho acumulado (491 499 features de testes anteriores)
  e o `bbox_query` marcou **28,0 ms** (+130% vs baseline). Repetindo em banco
  limpo, voltou a **~12,6 ms**. A diferença vem do índice GIST global maior
  (a query filtra por `layer_id`), não do código: por isso o protocolo desta
  re-execução cria um database limpo por rodada.

### Bench de render da GUI (headless, mesma rodada)

`cargo bench -p ta-gui --bench map_render` (dados `testdata/bench/` seed 42;
5 warm-up + 120 frames; 1280×800). Comparação com `benches/map_render.md`
(2026-10-16):

| cenário | mean (ms) | p95 (ms) | max (ms) | baseline mean (2026-10-16) | Δ mean |
|---|---|---|---|---|---|
| `10k_poligonos` | 1,76 | 1,89 | 2,03 | 2,06 | **−14,6%** |
| `50k_pontos` | 2,47 | 2,54 | 10,87 | 2,53 | **−2,4%** |
| `ambos_mais_raster_1024` | 5,53 | 5,91 | 6,11 | 6,96 | **−20,5%** |
| `raster_stress_8192` | 0,01 | 0,01 | 0,02 | 0,01 | 0,0% |

Carga 10k+50k em 251 ms (baseline: 237 ms); RSS Δ 62 MiB. Nenhum cenário
chega perto do gatilho de 33 ms (p95 pior = 5,91 ms, ~5,6× de folga), então a
Fase 3b continua **não disparada**. GPU segue não medida (headless mede só a
tesselação CPU do egui) — limitação já documentada em `map_render.md`.
