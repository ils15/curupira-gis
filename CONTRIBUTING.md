# CONTRIBUTING

Obrigado por contribuir com o **terraamazon-cleanroom-rust**. Antes de abrir
um PR, leia a barreira abaixo — ela é **inescindível** da participação.

## 🚫 Barreira dura clean-room (inegociável)

Este projeto é uma reimplementação independente (clean-room) e **proíbe
terminantemente**, para qualquer contribuidor humano ou agente de IA:

1. **Inspecionar** o binário, instalador ou pacote legado do TerraAmazon,
   incluindo especificamente o arquivo `TerraAmazon732.zip`
   (sha256 `50b2bc6a0b4fe137736af1b7ee59234003767be60e57a1cd5cb43ca117902e91`)
   **ou qualquer conteúdo dele derivado** (extração, descompactação, listagem
   de entradas, strings, recursos embutidos).
2. **Descompilar, desassemblar, tracejar, depurar ou fazer engenharia reversa**
   de qualquer binário/módulo legado.
3. **Copiar** código, pseudocódigo, recursos, ícones, strings ou layouts do
   produto legado para este repositório.

### Regras objetivas de rejeição de PR

Um PR é **rejeitado automaticamente** se citar, embutir ou evidenciar qualquer
um dos itens abaixo (produtos de engenharia reversa do produto legado):

- offsets de memória/arquivo, endereços, `hex dumps`;
- trechos de assembly ou bytecode;
- pseudocódigo decompilado (IDA/Ghidra/IDA-free/r2/objdump/etc.);
- hashes, nomes de símbolos internos ou recursos extraídos do binário legado;
- diffs "baseados no binário", "baseados no PE" ou derivações análogas.

A **única** fonte legítima de especificação é o **comportamento público**:
documentação de usuário, manuais, tutoriais públicos, capturas de tela e a
experiência observável do usuário final. Especifique comportamento, não
implementação.

### Dúvida prática

- Viu um comportamento no programa rodando e vai descrever "ao clicar X,
  acontece Y"? **OK** — é observação comportamental.
- Vai explicar *como* o binário faz internamente? **NÃO** — fora de escopo e
  fora da barreira.

## Licença

Toda contribuição é licenciada sob **MIT OR Apache-2.0** (ver `LICENSE-MIT` e
`LICENSE-APACHE`). Ao abrir um PR você declara que:

1. é o autor do código submetido, ou tem direito de submetê-lo sob essas
   licenças;
2. o código **não deriva** das fontes proibidas acima.

## Fluxo de desenvolvimento

- Toolchain: `rust-toolchain.toml` (stable, com `rustfmt` e `clippy`).
- Antes de abrir PR:
  ```sh
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  cargo deny check licenses advisories bans sources   # se tiver cargo-deny
  ```
- Banco de desenvolvimento: `docker compose up -d postgis` (PG16 + PostGIS 3.4).
- `Cargo.lock` é commitado (workspace com binários).
- Depespesas opcionais (GDAL, rusqlite/GPKG) ficam atrás de features — não
  habilite features pesadas no `default`.

## Estilo

- Mensagens de commit no formato Conventional Commits (`feat:`, `fix:`,
  `docs:`, `chore:` …).
- Documentação de módulos (`//!`) em pt-BR é aceita e incentivada; identificadores
  de código em inglês.
