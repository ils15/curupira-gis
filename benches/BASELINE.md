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

| cenário | mean | [IC 95% da média] | o que mede |
|---|---|---|---|
| `bulk_insert/10k_poligonos` | **500,9 ms** | [435,1 ms – 578,7 ms] | COPY FROM STDIN de 10k polígonos + atributos (fallback batched INSERT) |
| `bbox_query/janela_viewport` | **12,13 ms** | [12,03 ms – 12,24 ms] | `&&` + GIST devolvendo ~4 000 feições (10°×4°) com geometria e attrs |
| `list_with_ids/10k` | **26,07 ms** | [25,83 ms – 26,43 ms] | leitura completa (id de linha + EWKB + attrs) dos 10k |
| `mvt_tile/z12_viewport` | **não medido** | — | `ST_AsMVT` — ver limitação abaixo |

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

# baseline (o output traz mean/IC por cenário)
TA_TEST_PG=1 TA_PG_DSN=postgres://ta:ta@127.0.0.1:55432/ta \
    cargo bench -p ta-db --bench ta_db_baseline
```

Sem `TA_TEST_PG=1` o harness imprime o motivo e sai 0 — `cargo bench` nunca
quebra por ausência de banco. No CI o step roda com `continue-on-error: true`
(não-gating), no job que já tem o service PostGIS.
