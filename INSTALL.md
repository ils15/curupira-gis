# INSTALL — Curupira GIS (a partir do código-fonte)

Este documento cobre **desenvolvimento e execução local**. Para os binários de
release (Windows/macOS/Linux), use a página de
[Releases](https://github.com/ils15/curupira-gis/releases) — builds `v*` são
publicados como **draft** até revisão humana; confira o `SHA256SUMS`.

## 1. Pré-requisitos

| Componente | Versão/observação |
|---|---|
| **Rust** | stable, conforme `rust-toolchain.toml` (instale via <https://rustup.rs>). `cargo`, `rustfmt` e `clippy` são usados na verificação. |
| **GDAL** | 3.8 no host, link **dinâmico**: `sudo apt install libgdal-dev` (Debian/Ubuntu) ou `brew install gdal` (macOS). O `ta-cli doctor` confere e aponta o que falta. |
| **PostgreSQL** | **16** com **PostGIS 3.4**. Duas formas de obter: **docker** *ou* **conda** (fallback). |
| **Display** | Necessário apenas para a **GUI** (`DISPLAY` ou `WAYLAND_DISPLAY`). CLI e testes rodam headless. |

Não é preciso `DATABASE_URL` para **compilar**: as migrações são embutidas no
binário (`sqlx::migrate!`), não resolvidas em tempo de build. A conexão é
resolvida em runtime (`--dsn` > `TA_PG_DSN` > `TA_DATABASE_URL` na GUI >
default `postgres://ta:ta@localhost:5432/ta`).

## 2. Provisionamento em um comando

```sh
bash scripts/dev-up.sh
```

O script:

1. escolhe o banco — se `TA_PG_DSN` existir usa; senão, se a porta 5432
   escutar usa; senão tenta **docker** (`docker compose up -d postgis`); senão
   cai no **conda** (`scripts/test-pg.sh --no-tests`, porta **55432**);
2. compila o `ta-cli`;
3. `doctor` (informativo) → `migrate` (schema idempotente) → `doctor`
   (**reprova** o script se algo falhar) → `seed-demo`;
4. imprime a GUI e o DSN (com a senha **mascarada**: `postgres://ta:***@…`).

É idempotente: rodar 2× não sobe banco duplicado nem duplica o demo.

### 2b. GUI

```sh
bash scripts/dev-up.sh --gui
# ou, com o banco já no ar:
TA_DATABASE_URL='postgres://ta:ta@127.0.0.1:55432/ta' cargo run -p ta-gui
```

A GUI **exige display**. Sem `DISPLAY`/`WAYLAND_DISPLAY` o binário encerra com:

```
WinitEventLoop(Os(... "neither WAYLAND_DISPLAY nor WAYLAND_SOCKET nor DISPLAY is set."))
```

Em servidor/CI, use os testes headless (`cargo test -p ta-gui`) em vez de abrir
a janela.

### 2c. Manual (sem o script)

Com docker:

```sh
cp .env.example .env          # opcional: TA_PG_USER / TA_PG_PASSWORD
docker compose up -d postgis  # postgis/postgis:16-3.4 na porta 5432, banco `ta`
export TA_PG_DSN='postgres://ta:ta@localhost:5432/ta'
```

Sem docker (host com sudo bloqueado), cluster efêmero via conda-forge:

```sh
bash scripts/test-pg.sh --no-tests   # imprime o DSN e o comando de parada
export TA_PG_DSN='postgres://ta:ta@127.0.0.1:55432/ta'
```

Depois, em qualquer um dos casos:

```sh
cargo build -p ta-cli
target/debug/ta-cli doctor      # exit 0 = ambiente ok
target/debug/ta-cli migrate     # schema (idempotente)
target/debug/ta-cli seed-demo   # projeto `demo`
```

## 3. Migrações

As migrações vivem em `crates/ta-db/migrations/` (`0001`–`0014`) e são
**embutidas** no binário. Aplicação:

- automática no primeiro comando que abre o banco (`doctor`, `migrate`,
  `seed-demo`, CLI do fluxo, GUI); ou
- explícita:

```sh
ta-cli migrate --to postgres://ta:ta@127.0.0.1:55432/ta
ta-cli migrate --to … --dry-run          # só lista
```

O controle é a tabela `_sqlx_migrations`; reaplicar é idempotente
(`nada pendente`). O `doctor` mostra `migrações 14/14 aplicadas`.

### Privilégio de `spatial_ref_sys` (role separado)

A migração `0007_grid_srid` cria `cell_layers.srid → spatial_ref_sys(srid)`.
Se a extensão PostGIS foi criada por **outro role** (ex.: `postgres`) e a
aplicação usa um role diferente, a migração falha com `permission denied`.
Resolva de uma das duas formas:

```sql
-- (a) recomendado: criar a extensão com o role da aplicação
CREATE EXTENSION IF NOT EXISTS postgis;

-- (b) se a extensão já existe e é de outro role:
GRANT REFERENCES ON spatial_ref_sys TO <app_role>;
```

## 4. Testes

Suíte unitária + integração sem banco (testes PG imprimem “skipped” e passam):

```sh
cargo test --workspace
```

Suíte com **PostGIS real** (gated por `TA_TEST_PG=1`):

```sh
TA_TEST_PG=1 TA_PG_DSN=postgres://ta:ta@127.0.0.1:55432/ta \
    cargo test --workspace
```

Atalho que sobe o cluster, roda a suíte gated e derruba:

```sh
bash scripts/test-pg.sh
```

> O `test-pg.sh` **recusa** se a porta `55432` já estiver em uso
> (`porta 55432 já está em uso`). Nesse caso reuse o cluster existente com o
> `TA_TEST_PG=1 …` acima, ou escolha outra porta:
> `TA_PG_PORT=55433 bash scripts/test-pg.sh`.

Verificação de qualidade completa:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

## 5. Build de release (sem `DATABASE_URL`)

```sh
cargo build --release -p ta-gui -p ta-cli
# binários: target/release/curupira-gis e target/release/ta-cli
```

Não há `DATABASE_URL` em tempo de compilação: as migrações são embutidas. O
build de release **não** conecta ao banco; a conexão é escolhida em runtime
(diálogo da GUI ou `--dsn`/`TA_PG_DSN`).

Publicação: a tag de release precisa bater com `[workspace.package] version`
do `Cargo.toml` (ex.: `v0.1.0` == versão `0.1.0`); o workflow de release
**falha** se não bater.

## 6. Troubleshooting

| Sintoma | Causa / solução |
|---|---|
| `porta 55432 já está em uso` | Já há cluster conda/dev no ar. Reuse com `TA_PG_DSN=postgres://ta:ta@127.0.0.1:55432/ta` ou use `TA_PG_PORT=55433`. |
| `doctor` sai **1** | Leia as linhas `!`/`✗`: cada uma traz a ação. Rode `ta-cli migrate` antes de concluir que falta schema. |
| `! mvt ST_AsMVT indisponível` | PostGIS sem **protobuf-c** (build conda). Tiles vetoriais ficam off; o resto funciona. Use a imagem `postgis/postgis:16-3.4` para MVT. |
| `permission denied for ... spatial_ref_sys` | Ver §3 (`GRANT REFERENCES ON spatial_ref_sys`). |
| GUI fecha com `WinitEventLoop … DISPLAY` | Não há display. Rode em desktop ou use os testes headless. |
| Senha aparece mascarada | É intencional: saídas e `logs` mostram `postgres://user:***@host`. O valor real só existe no ambiente do processo. |
| `docker` ausente | Esperado: o `dev-up.sh` cai no conda; `scripts/test-pg.sh` provisiona PG16+PostGIS no `$HOME` (única instalação do workflow). |
| GDAL não encontrada no build | Instale `libgdal-dev` (Linux) / `gdal` (brew). O link é dinâmico; o `doctor` confirma a versão. |

## 7. Onde está o quê

- Fluxo e comandos: [`README.md`](README.md) §“Fluxo completo pela CLI”.
- Matriz de paridade e limitações: [`docs/PARITY.md`](docs/PARITY.md).
- Estado das fases e dívidas: `.pantheon/deepwork/terraamazon-cleanroom-rust/STATUS.md`.
