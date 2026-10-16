# NOTICE

## Declaração clean-room

**terraamazon-cleanroom-rust** é uma **reimplementação independente**, escrita
do zero, do escopo funcional de um GIS desktop de projetos/banco geoespacial.
O projeto **não contém código derivado** de TerraAmazon, TerraLib, TerraView
nem de nenhum artefato binário ou fonte associado a essas plataformas.

- As referências usadas durante o desenvolvimento são **exclusivamente
  comportamentais e públicas**: documentação de usuário, capturas de tela
  públicas, descrição de funcionalidades e especificações abertas (OGC,
  PostGIS, EPSG, formatos de arquivo documentados).
- Nenhum desenvolvedor ou agente (humano ou IA) participante deste repositório
  deve ter acesso ao binário, instalador, código-fonte ou conteúdo descompactado
  do produto legado. Ver `CONTRIBUTING.md` para a barreira dura.
- Qualquer semelhança de comportamento decorre da especificação funcional
  pública perseguida (paridade comportamental "tipo TerraAmazon"), não de
  derivação de código.

## Componentes de terceiros (runtime/build)

| Componente | Papel | Licença (própria) |
|---|---|---|
| PostgreSQL 16 + PostGIS 3.4 | banco primário | PostgreSQL / GPL+ (extensão) |
| GDAL 3.8 (ligação dinâmica, opcional) | IO raster/vetorial | MIT |
| egui / eframe | GUI | MIT OR Apache-2.0 |
| sqlx | persistência async | MIT OR Apache-2.0 |
| geozero, geo-types | conversões geoespaciais | MIT OR Apache-2.0 |
| rust crate ecosystem (ver `Cargo.lock`) | build | — |

Cada componente retém sua licença original; este repositório não os distribui
vendidos ou modificados. O código deste repositório é distribuído sob
**MIT OR Apache-2.0** (ver `LICENSE-MIT` e `LICENSE-APACHE`).
