# INSTALL — Curupira GIS

Instalação por sistema operacional. Binários de release ficam em
<https://github.com/ils15/curupira-gis/releases> (builds `v*` são publicados
como **draft**; um mantenedor marca "Publish" após revisão — ver
`SHA256SUMS` para conferir integridade).

Componentes de uma release:

| Arquivo | Conteúdo |
|---|---|
| `curupira-gis-v*-x86_64-unknown-linux-gnu.tar.gz` | `curupira-gis` + `ta-cli` (Linux; GDAL do sistema) |
| `curupira-gis_x64.deb` | pacote .deb (Linux; os dois binários em `/usr/bin`; GDAL do sistema via `depends` resolvido pelo dpkg) |
| `curupira-gis-v*-x86_64-pc-windows-msvc.zip` | `curupira-gis.exe` + `ta-cli.exe` + DLLs GDAL/PROJ embutidas + `share\` |
| `CurupiraGIS-Setup-x64.exe` | instalador Windows (Inno Setup; mesmos binários/DLLs/dados do zip + atalhos e desinstalador) |
| `CurupiraGIS-macos-arm64.zip` | `CurupiraGIS.app` (Apple Silicon; GDAL via Homebrew embutida no bundle; **não assinado**) |
| `SHA256SUMS` | checksums sha256 de todos os artefatos |
| `*.cdx.json` | SBOM CycloneDX por crate do workspace |

---

## Windows 10/11 (x64)

### 1. Curupira GIS — instalador (recomendado)

1. Baixe `CurupiraGIS-Setup-x64.exe` da release.
2. Confira o hash: `Get-FileHash <arquivo>.exe -Algorithm SHA256` e compare
   com `SHA256SUMS`.
3. Execute o instalador (pt-BR ou en). Ele instala em `C:\Program Files\
   Curupira GIS` (ou `%LOCALAPPDATA%\Programs\…` no modo "só p/ mim"), com
   atalhos no menu iniciar (e na área de trabalho, opcional), página de
   licença (MIT) e desinstalador no Painel de Controle.
4. GDAL/PROJ e os dados (`share\gdal`, `share\proj`) vão **dentro** da pasta
   de instalação — o app aponta para eles automaticamente; não instale GDAL.
5. Rode `Curupira GIS` pelo atalho (ou `curupira-gis.exe` direto).

> O `ta-cli.exe` fica na mesma pasta — adicione-a ao `PATH` manualmente se
> quiser chamá-lo de qualquer terminal.

### 1b. Curupira GIS — zip portátil (alternativa, sem instalador)

1. Baixe `curupira-gis-v*-x86_64-pc-windows-msvc.zip` da release.
2. Confira o hash: `Get-FileHash <arquivo>.zip -Algorithm SHA256` e compare
   com `SHA256SUMS`.
3. Extraia em uma pasta sem espaços se possível (ex.: `C:\Curupira`).
   O zip já contém **GDAL + PROJ com dados** (`share\gdal`, `share\proj`) —
   o app aponta para eles automaticamente ao iniciar; não instale GDAL.
4. Rode `curupira-gis.exe`.

### 2. PostgreSQL 16 + PostGIS 3.4

1. Instale **PostgreSQL 16** pelo instalador EDB:
   <https://www.enterprisedb.com/downloads/postgres-postgresql-downloads>
2. Ao final da instalação, deixe o **StackBuilder** abrir e marque
   **PostGIS 3.4 for PostgreSQL 16** (componentes espaciais + GDAL própria
   do PostGIS — não conflita com a embutida do Curupira).
3. Crie o banco do app (psql ou pgAdmin):

   ```sql
   CREATE DATABASE curupira;
   ```

   O schema é criado automaticamente pelo app/CLI na primeira conexão.

### 3. Conexão

- **Diálogo do app**: menu *Ajuda → Conectar banco…* — preencha host, porta,
  usuário, senha e banco, ou cole a URL direta.
- **Variável de ambiente** (pré-preenche o diálogo):

  ```powershell
  [Environment]::SetEnvironmentVariable(
    "TA_DATABASE_URL", "postgres://postgres:SUA_SENHA@localhost:5432/curupira", "User")
  ```

- Para o `ta-cli` use `TA_PG_DSN` (mesma URL) ou `--dsn`.

---

## Linux (x86_64; Debian/Ubuntu como referência)

### 1. Dependências de sistema

```sh
sudo apt install -y libgdal-dev gdal-bin   # GDAL 3.8+ (link dinâmico)
```

### 2. Curupira GIS

- **Pacote .deb** (Ubuntu/Debian, recomendado): baixe `curupira-gis_x64.deb`
  da release e instale — os binários vão para `/usr/bin` e a documentação
  para `/usr/share/doc/curupira-gis/`:

  ```sh
  sudo apt install ./curupira-gis_x64.deb
  # (instala libgdalXX do repositório automaticamente — dependência declarada)
  ```

- **Release tarball**: baixe
  `curupira-gis-v*-x86_64-unknown-linux-gnu.tar.gz`, verifique
  `sha256sum -c SHA256SUMS --ignore-missing`, extraia e coloque os binários
  no `PATH`:

  ```sh
  tar xzf curupira-gis-v*.tar.gz
  sudo install -m755 curupira-gis ta-cli /usr/local/bin/
  ```

- **Do fonte** (qualquer Linux):

  ```sh
  sudo apt install -y build-essential pkg-config
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh  # rustup
  cargo build --release --workspace
  # binários: target/release/curupira-gis e target/release/ta-cli
  ```

### 3. PostgreSQL 16 + PostGIS 3.4 (docker compose, recomendado)

```sh
cp .env.example .env      # ajuste usuário/senha se quiser
docker compose up -d postgis
# conexão: postgres://ta:ta@localhost:5432/ta
```

Alternativa nativa: `sudo apt install -y postgresql-16 postgis postgresql-16-postgis-3`
(ou os pacotes do [PGDG](https://www.postgresql.org/download/linux/) /
[PostGIS](https://postgis.net/documentation/getting_started/)), depois
`CREATE DATABASE curupira;` e `CREATE EXTENSION postgis;` (o app tenta criar
a extensão se o usuário tiver permissão).

### 4. Conexão

- Diálogo do app (*Conectar banco…*), ou
- `export TA_DATABASE_URL="postgres://ta:ta@localhost:5432/ta"` (GUI), /
  `TA_PG_DSN` (CLI).

---

## macOS (Apple Silicon / arm64)

> Alpha: bundle **não assinado e não notarizado** — o Gatekeeper bloqueia na
> primeira abertura. Use **botão direito no CurupiraGIS.app → Abrir →
> Abrir** (uma vez só; depois o app abre normal).

1. Baixe `CurupiraGIS-macos-arm64.zip` da release e confira o hash:
   `shasum -a 256 <arquivo>.zip` — compare com `SHA256SUMS`.
2. Extraia (duplo clique no zip) e arraste `CurupiraGIS.app` para
   `/Applications` (ou rode de onde estiver).
3. Botão direito → **Abrir** na primeira vez (nota do Gatekeeper acima).
   A GDAL/PROJ embutida no bundle (Frameworks + dados no Resources do app
   via Homebrew) dispensa instalar GDAL.
4. Para o `ta-cli` via terminal:
   `/Applications/CurupiraGIS.app/Contents/MacOS/ta-cli`.
5. Banco: PostgreSQL 16 + PostGIS 3.4 (ex.: `brew install postgresql@16` +
   PostGIS, ou docker compose do repo).

---

## Primeiro run (todos os OS)

1. **Conectar** — abra o Curupira GIS; no diálogo de conexão use a URL do
   passo anterior (ou confie em `TA_DATABASE_URL`). O app aplica as migrações
   no primeiro uso.
2. **Criar projeto** — botão *Projetos*: cada projeto é um namespace de
   camadas no banco (SRID de trabalho 4674 — SIRGAS 2000).
3. **Importar um fixture** — menu *Camada → Importar…* (ou CLI):

   ```sh
   # vetores de exemplo que acompanham o repo:
   ta-cli import-vector --input testdata/fixtures_vector.geojson --project minha-area
   ta-cli import-vector --input testdata/fixtures_points.csv --project minha-area
   # raster (metadados no banco; pixels ficam no filesystem):
   ta-cli import-raster --input testdata/fixtures_raster.tif --project minha-area
   ```

4. **Ver no mapa** — a camada importada aparece no dock; *Zoom to extent*
   enquadra; a tabela atributiva abre com duplo clique na camada.
5. (Opcional) **Basemap WMS** — ative a feature `wms` no build e configure
   um servidor WMS 1.3.0; o cache em disco evita recomprar tiles iguais.

---

## Solução de problemas

| Sintoma | Causa provável | Correção |
|---|---|---|
| `gdal.dll` / `libgdal.so.X` não encontrado | GDAL ausente (Linux/build do fonte) | `apt install libgdal-dev` / instale GDAL 3.8+ |
| App abre, raster não projeta | `share\proj` removido do zip ou `PROJ_LIB` sobrescrito | reinstale o zip; não defina `PROJ_LIB` manualmente |
| `no pg_hba.conf entry` / auth falha | senha/host errados no DSN | revise `TA_DATABASE_URL`; no Windows a senha é a do instalador EDB |
| `postgis` extension ausente | StackBuilder não instalou PostGIS | rode `CREATE EXTENSION postgis;` como superuser |
| Fonte/acentos quebrados no CSV | encoding ≠ UTF-8 | use `--encoding latin1` no import (CLI) |
